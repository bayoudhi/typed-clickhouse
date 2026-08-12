#[macro_use]
pub(crate) mod display;

mod commands;
pub mod logger;
pub mod routines;
use crate::cli::routines::seed_data;
pub mod settings;
use crate::utilities::constants;
use clap::Parser;
use commands::{Commands, DbCommands, GenerateCommand};
use config::ConfigError;
use display::with_spinner_completion;
use regex::Regex;
use routines::auth::{display_hash_token_result, generate_hash_token};
use routines::build::build_package;
use routines::peek::peek;
use routines::query::query;
use tracing::{debug, info, warn};

use settings::Settings;
use std::sync::Arc;

use crate::cli::display::{Message, MessageType};
use crate::cli::routines::logs::{follow_logs, show_logs};
use crate::cli::routines::{RoutineFailure, RoutineSuccess};
use crate::cli::settings::user_directory;
use crate::framework::core::check::check_system_reqs;
use crate::framework::core::infrastructure_map::InfrastructureMap;
use crate::infrastructure::olap::clickhouse::config::parse_clickhouse_connection_string;
use crate::project::Project;
use crate::utilities::constants::KEY_REMOTE_CLICKHOUSE_URL;
use crate::utilities::constants::{
    ENV_CLICKHOUSE_URL, MIGRATION_AFTER_STATE_FILE, MIGRATION_BEFORE_STATE_FILE, MIGRATION_FILE,
    PROJECT_NAME_ALLOW_PATTERN,
};
use crate::utilities::keyring::{KeyringSecretRepository, SecretRepository};

use crate::cli::commands::DbArgs;
use crate::cli::routines::code_generation::{db_pull, db_pull_from_remote};
use crate::cli::routines::ls::ls;
use crate::framework::core::migration_plan::MIGRATION_SCHEMA;
use crate::infrastructure::olap::clickhouse::config_resolver::resolve_remote_clickhouse;
use crate::utilities::constants::QUIET_STDOUT;
use anyhow::Result;
use std::sync::atomic::Ordering;

/// Generic prompt function with hints, default values, and better formatting
pub fn prompt_user(
    prompt_text: &str,
    default: Option<&str>,
    hint: Option<&str>,
) -> Result<String, RoutineFailure> {
    use std::io::{self, Write};

    // Build the prompt with proper formatting
    let mut full_prompt = String::new();

    // Add the main prompt text
    full_prompt.push_str(prompt_text);

    // Add default value if provided
    if let Some(default_value) = default {
        full_prompt.push_str(&format!(" (default: {})", default_value));
    }

    // Add hint if provided
    if let Some(hint_text) = hint {
        full_prompt.push_str(&format!("\n  💡 Hint: {}", hint_text));
    }

    // Add the prompt indicator
    full_prompt.push_str("\n> ");

    print!("{}", full_prompt);
    let _ = io::stdout().flush();
    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|e| {
        RoutineFailure::new(
            Message {
                action: "Init".to_string(),
                details: "Failed to prompt user".to_string(),
            },
            e,
        )
    })?;
    let trimmed = input.trim();

    // Return default if input is empty, otherwise return the trimmed input
    let result = if trimmed.is_empty() {
        default.unwrap_or("").to_string()
    } else {
        trimmed.to_string()
    };

    Ok(result)
}

/// Prompts user for password input with masked characters (shows * instead of typed chars)
///
/// Uses crossterm for terminal manipulation to hide the actual password input.
pub fn prompt_password(prompt_text: &str) -> Result<String, RoutineFailure> {
    use crossterm::{
        event::{read, Event, KeyCode, KeyModifiers},
        terminal::{disable_raw_mode, enable_raw_mode},
    };
    use std::io::{self, Write};

    // Print the prompt
    print!("{}\n> ", prompt_text);
    let _ = io::stdout().flush();

    // Enable raw mode to capture individual key presses
    enable_raw_mode().map_err(|e| {
        RoutineFailure::new(
            Message {
                action: "Password".to_string(),
                details: "Failed to enable terminal raw mode".to_string(),
            },
            e,
        )
    })?;

    let mut password = String::new();

    loop {
        match read() {
            Ok(Event::Key(key_event)) => {
                // Handle Ctrl+C to cancel
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && key_event.code == KeyCode::Char('c')
                {
                    let _ = disable_raw_mode();
                    println!();
                    return Err(RoutineFailure::error(Message {
                        action: "Password".to_string(),
                        details: "Input cancelled by user".to_string(),
                    }));
                }

                // Ignore control key combinations (Ctrl+V, Ctrl+A, etc.) to prevent
                // accidental character input. However, allow:
                // - ALT alone: macOS Option key for special characters
                // - CTRL+ALT: Windows AltGr for international keyboard characters
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && !key_event.modifiers.contains(KeyModifiers::ALT)
                {
                    continue;
                }

                match key_event.code {
                    KeyCode::Enter => {
                        let _ = disable_raw_mode();
                        println!(); // Move to next line after password entry
                        return Ok(password);
                    }
                    KeyCode::Backspace => {
                        if !password.is_empty() {
                            password.pop();
                            // Erase the last asterisk: move back, print space, move back again
                            print!("\x08 \x08");
                            let _ = io::stdout().flush();
                        }
                    }
                    KeyCode::Char(c) => {
                        password.push(c);
                        print!("*"); // Show asterisk instead of actual character
                        let _ = io::stdout().flush();
                    }
                    _ => {} // Ignore other keys
                }
            }
            Ok(_) => {} // Ignore non-key events
            Err(e) => {
                let _ = disable_raw_mode();
                return Err(RoutineFailure::new(
                    Message {
                        action: "Password".to_string(),
                        details: "Failed to read input".to_string(),
                    },
                    e,
                ));
            }
        }
    }
}

#[derive(Parser)]
#[command(
    author,
    version = constants::CLI_VERSION,
    about = "typed-clickhouse is a type-safe, code-first tool for declaring ClickHouse tables and views as TypeScript types, and generating and applying migrations against a live database.",
    long_about = None,
    arg_required_else_help(true),
    next_display_order = None
)]
pub struct Cli {
    /// Turn debugging information on
    #[arg(short, long)]
    debug: bool,

    /// Print backtraces for all errors (same as RUST_LIB_BACKTRACE=1)
    #[arg(
        long,
        global = true,
        help = "Print backtraces for all errors (same as RUST_LIB_BACKTRACE=1)"
    )]
    pub backtrace: bool,

    #[command(subcommand)]
    pub command: Commands,
}

/// Determines the runtime environment from the CLI command
fn determine_environment(command: &Commands) -> crate::utilities::dotenv::RuntimeEnvironment {
    use crate::utilities::dotenv::RuntimeEnvironment;

    match command {
        // Production commands
        Commands::Build { .. } => RuntimeEnvironment::Production,

        // All other commands default to development
        _ => RuntimeEnvironment::Development,
    }
}

pub fn load_project(command: &Commands) -> Result<Project, RoutineFailure> {
    let environment = determine_environment(command);
    Project::load_from_current_dir(environment).map_err(|e| match e {
        ConfigError::Foreign(_) => RoutineFailure::error(Message {
            action: "Loading".to_string(),
            details: "No tch.config.toml found. Run this command from a project directory."
                .to_string(),
        }),
        _ => RoutineFailure::error(Message {
            action: "Loading".to_string(),
            details: format!("Please validate the project's configs: {e:?}"),
        }),
    })
}

fn check_project_name(name: &str) -> Result<(), RoutineFailure> {
    // Special case: Allow "." as a valid project name to indicate current directory
    if name == "." {
        return Ok(());
    }

    let project_name_regex = Regex::new(PROJECT_NAME_ALLOW_PATTERN).unwrap();
    if !project_name_regex.is_match(name) {
        return Err(RoutineFailure::error(Message {
            action: "Init".to_string(),
            details: format!(
                "Project name should match the following: {PROJECT_NAME_ALLOW_PATTERN}"
            ),
        }));
    }
    Ok(())
}

/// Resolves ClickHouse URL from flag and environment variable (no Redis validation)
/// Use this for commands that only need ClickHouse access (e.g., db pull)
fn resolve_clickhouse_url(clickhouse_url: Option<&str>) -> Option<String> {
    use crate::utilities::constants::ENV_CLICKHOUSE_URL;

    // Resolve ClickHouse URL from flag or env var
    let clickhouse_url_from_env = std::env::var(ENV_CLICKHOUSE_URL).ok();
    clickhouse_url.map(String::from).or(clickhouse_url_from_env)
}

/// Override project's ClickHouse config from flag/env var url
/// This allows the user to run these commands against other environments
/// while keeping the project config focused on dev infrastructure
fn override_project_config_from_url(
    project: &mut Project,
    clickhouse_url: &str,
) -> Result<(), RoutineFailure> {
    let clickhouse_config = parse_clickhouse_connection_string(clickhouse_url).map_err(|e| {
        RoutineFailure::new(
            Message::new(
                "Configuration".to_string(),
                "Failed to parse ClickHouse URL".to_string(),
            ),
            e,
        )
    })?;

    let clusters = project.clickhouse_config.clusters.clone();
    let additional_databases = project.clickhouse_config.additional_databases.clone();

    project.clickhouse_config = clickhouse_config;
    project.clickhouse_config.clusters = clusters;
    project.clickhouse_config.additional_databases = additional_databases;

    info!(
        "Overriding project ClickHouse config from CLI: database = {}",
        project.clickhouse_config.db_name
    );

    Ok(())
}

pub async fn top_command_handler(
    settings: Settings,
    commands: &Commands,
) -> Result<RoutineSuccess, RoutineFailure> {
    match commands {
        // This command is used to check the project for errors that are not related to runtime
        // For example, it checks that the project is valid and that all the primitives are loaded
        // It is used in the build process to ensure that the project is valid while building docker images
        Commands::Check { write_infra_map } => {
            info!(
                "Running check command with write_infra_map: {}",
                *write_infra_map
            );
            let project_arc = Arc::new(load_project(commands)?);

            check_project_name(&project_arc.name())?;

            check_system_reqs(&project_arc.language_project_config)
                .await
                .map_err(|e| {
                    RoutineFailure::error(Message {
                        action: "System".to_string(),
                        details: format!("Failed to validate system requirements: {e:?}"),
                    })
                })?;

            debug!("Loading InfrastructureMap from user code");
            // Don't resolve credentials for typed-clickhouse check - avoids baking into Docker
            let infra_map = InfrastructureMap::load_from_user_code(&project_arc, false)
                .await
                .map_err(|e| {
                    RoutineFailure::error(Message {
                        action: "Build".to_string(),
                        details: format!("Failed to load InfrastructureMap: {e:?}"),
                    })
                })?;

            if *write_infra_map {
                let json_path = project_arc
                    .internal_dir_with_routine_failure_err()?
                    .join("infrastructure_map.json");

                infra_map.save_to_json(&json_path).map_err(|e| {
                    RoutineFailure::new(
                        Message::new(
                            "Failed".to_string(),
                            "to save InfrastructureMap as JSON".to_string(),
                        ),
                        e,
                    )
                })?;
            }

            Ok(RoutineSuccess::success(Message::new(
                "Checked".to_string(),
                "No Errors found".to_string(),
            )))
        }
        Commands::Build {} => {
            info!("Running build command");
            let project_arc = Arc::new(load_project(commands)?);
            check_project_name(&project_arc.name())?;

            let package_path = with_spinner_completion(
                "Bundling deployment package",
                "Package bundled successfully",
                || {
                    build_package(&project_arc).map_err(|e| {
                        RoutineFailure::error(Message {
                            action: "Build".to_string(),
                            details: format!("Failed to build package: {e:?}"),
                        })
                    })
                },
                !project_arc.is_production,
            )?;

            Ok(RoutineSuccess::success(Message::new(
                "Built".to_string(),
                format!("Package available at {}", package_path.display()),
            )))
        }
        Commands::Generate(generate) => match &generate.command {
            Some(GenerateCommand::HashToken { json }) => {
                info!("Running generate hash token command");

                // Set QUIET_STDOUT early to redirect any messages (like config warnings)
                // to stderr, keeping stdout clean for JSON output
                if *json {
                    QUIET_STDOUT.store(true, Ordering::Relaxed);
                }

                let project = load_project(commands)?;
                let project_arc = Arc::new(project);

                check_project_name(&project_arc.name())?;
                let result = generate_hash_token();

                if *json {
                    println!("{}", serde_json::to_string_pretty(&result).unwrap());
                } else {
                    display_hash_token_result(&result);
                }

                Ok(RoutineSuccess::success(Message::new(
                    "Token".to_string(),
                    "Generated successfully".to_string(),
                )))
            }
            Some(GenerateCommand::Migration {
                clickhouse_url,
                save,
            }) => {
                info!("Running generate migration command");

                let mut project = load_project(commands)?;

                check_project_name(&project.name())?;

                let ch_url =
                    resolve_clickhouse_url(clickhouse_url.as_deref()).ok_or_else(|| {
                        RoutineFailure::error(Message {
                            action: "Configuration".to_string(),
                            details: format!(
                                "--clickhouse-url required (or set {} environment variable)",
                                ENV_CLICKHOUSE_URL
                            ),
                        })
                    })?;

                override_project_config_from_url(&mut project, &ch_url)?;

                let result = routines::remote_gen_migration(&project, &ch_url).await;

                let result = result.map_err(|e| {
                    RoutineFailure::new(
                        Message {
                            action: "Plan".to_string(),
                            details: "Failed to generate migration plan".to_string(),
                        },
                        e,
                    )
                })?;

                let plan_yaml = result.db_migration.to_yaml().map_err(|e| {
                    RoutineFailure::new(
                        Message {
                            action: "Plan".to_string(),
                            details: "Failed to serialize".to_string(),
                        },
                        e,
                    )
                })?;

                if *save {
                    std::fs::create_dir_all("./migrations").map_err(|e| {
                        RoutineFailure::new(
                            Message::new(
                                "Migration".to_string(),
                                "plan writing failed.".to_string(),
                            ),
                            e,
                        )
                    })?;

                    if let Err(e) = std::fs::write(
                        project
                            .internal_dir_with_routine_failure_err()?
                            .join("migration_schema.json"),
                        MIGRATION_SCHEMA,
                    ) {
                        warn!("Error writing migration schema file: {e:?}");
                    };
                    // Prepend YAML language server schema directive for better editor support
                    let plan_yaml_with_header = format!(
                        "# yaml-language-server: $schema=../.tch/migration_schema.json\n\n{}",
                        plan_yaml
                    );
                    std::fs::write(MIGRATION_FILE, plan_yaml_with_header.as_str()).map_err(
                        |e| {
                            RoutineFailure::new(
                                Message::new(
                                    "Migration".to_string(),
                                    "plan writing failed.".to_string(),
                                ),
                                e,
                            )
                        },
                    )?;
                    std::fs::write(
                        MIGRATION_BEFORE_STATE_FILE,
                        serde_json::to_string_pretty(&result.remote_state).map_err(|e| {
                            RoutineFailure::new(
                                Message::new(
                                    "Error".to_string(),
                                    "serializing remote state.".to_string(),
                                ),
                                e,
                            )
                        })?,
                    )
                    .map_err(|e| {
                        RoutineFailure::new(
                            Message::new(
                                "Migration".to_string(),
                                "plan writing failed.".to_string(),
                            ),
                            e,
                        )
                    })?;
                    std::fs::write(
                        MIGRATION_AFTER_STATE_FILE,
                        serde_json::to_string_pretty(&result.local_infra_map).map_err(|e| {
                            RoutineFailure::new(
                                Message::new(
                                    "Error".to_string(),
                                    "serializing local state.".to_string(),
                                ),
                                e,
                            )
                        })?,
                    )
                    .map_err(|e| {
                        RoutineFailure::new(
                            Message::new(
                                "Migration".to_string(),
                                "plan writing failed.".to_string(),
                            ),
                            e,
                        )
                    })?;
                } else {
                    println!("Changes: \n\n{}", plan_yaml);
                }

                Ok(RoutineSuccess::success(Message::new(
                    "Migration".to_string(),
                    "generated".to_string(),
                )))
            }
            None => Err(RoutineFailure::error(Message {
                action: "Generate".to_string(),
                details: "Please provide a subcommand".to_string(),
            })),
        },
        Commands::Plan {
            clickhouse_url,
            json,
        } => {
            info!("Running plan command");

            // Set QUIET_STDOUT early to redirect any messages (like config warnings)
            // to stderr, keeping stdout clean for JSON output
            if *json {
                QUIET_STDOUT.store(true, Ordering::Relaxed);
            }

            let project = load_project(commands)?;

            check_project_name(&project.name())?;

            let ch_url = resolve_clickhouse_url(clickhouse_url.as_deref()).ok_or_else(|| {
                RoutineFailure::error(Message {
                    action: "Configuration".to_string(),
                    details: format!(
                        "--clickhouse-url required (or set {} environment variable)",
                        ENV_CLICKHOUSE_URL
                    ),
                })
            })?;

            let result = routines::remote_plan(&project, &ch_url, *json).await;

            result.map_err(|e| {
                RoutineFailure::error(Message {
                    action: "Plan".to_string(),
                    details: format!("Failed to plan changes: {e:?}"),
                })
            })?;

            // When --json is used, output is already printed, so suppress success message
            if *json {
                Ok(RoutineSuccess::success(Message::new(
                    "".to_string(),
                    "".to_string(),
                )))
            } else {
                Ok(RoutineSuccess::success(Message::new(
                    "Plan".to_string(),
                    "Successfully planned changes to the infrastructure".to_string(),
                )))
            }
        }
        Commands::Migrate { clickhouse_url } => {
            info!("Running migrate command");
            let mut project = load_project(commands)?;

            check_project_name(&project.name())?;

            let resolved_clickhouse_url = resolve_clickhouse_url(clickhouse_url.as_deref())
                .ok_or_else(|| {
                    RoutineFailure::error(Message {
                        action: "Configuration".to_string(),
                        details: format!(
                            "--clickhouse-url required (or set {} environment variable)",
                            ENV_CLICKHOUSE_URL
                        ),
                    })
                })?;

            override_project_config_from_url(&mut project, &resolved_clickhouse_url)?;

            routines::migrate::execute_migration(&project).await?;

            Ok(RoutineSuccess::success(Message::new(
                "Migrate".to_string(),
                "Successfully executed migration plan".to_string(),
            )))
        }
        Commands::Logs { tail, filter } => {
            info!("Running logs command");

            let project = load_project(commands)?;

            check_project_name(&project.name())?;

            let log_file_path = chrono::Local::now()
                .format(&settings.logger.log_file_date_format)
                .to_string();

            let log_file_path = user_directory()
                .map_err(|e| {
                    RoutineFailure::new(
                        Message::new("Failed".to_string(), "to resolve log directory".to_string()),
                        e,
                    )
                })?
                .join(log_file_path)
                .to_str()
                .unwrap()
                .to_string();

            let filter_value = filter.clone().unwrap_or_else(|| "".to_string());

            if *tail {
                follow_logs(log_file_path, filter_value)
            } else {
                show_logs(log_file_path, filter_value)
            }
        }
        Commands::Ls { _type, name, json } => {
            info!("Running ls command");

            let project = load_project(commands)?;
            let project_arc = Arc::new(project);

            ls(&project_arc, _type.as_deref(), name.as_deref(), *json).await
        }
        Commands::Peek { name, limit, file } => {
            info!("Running peek command");

            let project = load_project(commands)?;
            let project_arc = Arc::new(project);

            peek(project_arc, name, *limit, file.clone()).await
        }
        Commands::Db(DbArgs {
            command:
                DbCommands::Pull {
                    clickhouse_url,
                    file_path,
                },
        }) => {
            info!("Running db pull command");
            let project = load_project(commands)?;

            // Use resolve_clickhouse_url for env var fallback (db pull only needs ClickHouse, not Redis)
            let resolved_from_flag_or_env = resolve_clickhouse_url(clickhouse_url.as_deref());

            // Fall back to keyring if not provided via flag or env var
            match resolved_from_flag_or_env {
                Some(url) => {
                    db_pull(&url, &project, file_path.as_deref())
                        .await
                        .map_err(|e| {
                            RoutineFailure::new(
                                Message::new("DB Pull".to_string(), "failed".to_string()),
                                e,
                            )
                        })?;
                }
                None => {
                    // Try a URL previously saved to the keychain for this project
                    let repo = KeyringSecretRepository;
                    match repo.get(&project.name(), KEY_REMOTE_CLICKHOUSE_URL) {
                        Ok(Some(url)) => {
                            db_pull(&url, &project, file_path.as_deref())
                                .await
                                .map_err(|e| {
                                    RoutineFailure::new(
                                        Message::new("DB Pull".to_string(), "failed".to_string()),
                                        e,
                                    )
                                })?;
                        }
                        Ok(None) => {
                            // Try [dev.remote_clickhouse] config with keychain credentials
                            match resolve_remote_clickhouse(&project) {
                                Ok(Some(remote)) => {
                                    db_pull_from_remote(&remote, &project, file_path.as_deref())
                                        .await?;
                                }
                                Ok(None) => {
                                    return Err(RoutineFailure::error(Message {
                                        action: "DB Pull".to_string(),
                                        details: format!(
                                            "No ClickHouse connection found. Options:\n\
                                            1. Pass --clickhouse-url\n\
                                            2. Set {} environment variable\n\
                                            3. Configure [dev.remote_clickhouse] in tch.config.toml",
                                            ENV_CLICKHOUSE_URL
                                        ),
                                    }));
                                }
                                Err(e) => return Err(e),
                            }
                        }
                        Err(e) => {
                            return Err(RoutineFailure::error(Message {
                                action: "DB Pull".to_string(),
                                details: format!(
                                    "Failed to read saved ClickHouse URL from keychain: {e:?}"
                                ),
                            }));
                        }
                    }
                }
            };

            Ok(RoutineSuccess::success(Message::new(
                "DB Pull".to_string(),
                "External models refreshed".to_string(),
            )))
        }
        Commands::Seed(seed_args) => {
            let project = load_project(commands)?;

            seed_data::handle_seed_command(seed_args, &project).await
        }
        Commands::Truncate { tables, all, rows } => {
            let project = load_project(commands)?;
            routines::truncate_table::truncate_tables(&project, tables.clone(), *all, *rows).await
        }
        Commands::Query {
            query: sql,
            file,
            limit,
            format_query,
            prettify,
        } => {
            info!("Running query command");

            let project = load_project(commands)?;
            let project_arc = Arc::new(project);

            query(
                project_arc,
                sql.clone(),
                file.clone(),
                *limit,
                format_query.clone(),
                *prettify,
            )
            .await
        }
    }
}
