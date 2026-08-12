use crate::cli::display::{Message, MessageType};
use crate::cli::routines::RoutineFailure;
use crate::framework::core::infrastructure::table::Table;
use crate::framework::core::infrastructure_map::InfrastructureMap;
use crate::framework::core::partial_infrastructure_map::LifeCycle;
use crate::framework::languages::SupportedLanguages;
use crate::framework::typescript::generate::tables_to_typescript;
use crate::infrastructure::olap::clickhouse::remote::ClickHouseRemote;
use crate::infrastructure::olap::clickhouse::{create_readonly_client, ConfiguredDBClient};
use crate::infrastructure::olap::OlapOperations;
use crate::project::Project;
use crate::utilities::constants::TYPESCRIPT_EXTERNAL_FILE;
use clickhouse::Client;
use std::borrow::Cow;
use std::io::Write;
use tracing::debug;

// Shared helpers
pub async fn create_client_and_db(
    remote_url: &str,
) -> Result<(ConfiguredDBClient, String), RoutineFailure> {
    use crate::infrastructure::olap::clickhouse::config::parse_clickhouse_connection_string_with_metadata;

    // Parse the connection string with metadata
    let parsed = parse_clickhouse_connection_string_with_metadata(remote_url).map_err(|e| {
        RoutineFailure::new(
            Message::new(
                "Invalid URL".to_string(),
                format!("Failed to parse ClickHouse URL '{remote_url}'"),
            ),
            e,
        )
    })?;

    // Show user-facing message if native protocol was converted
    if parsed.was_native_protocol {
        debug!("Only HTTP(s) supported. Transforming native protocol connection string.");
        show_message!(
            MessageType::Highlight,
            Message {
                action: "Protocol".to_string(),
                details: format!(
                    "native protocol detected. Converting to HTTP(s): {}",
                    parsed.display_url
                ),
            }
        );
    }

    let mut config = parsed.config;

    // If database wasn't explicitly specified in URL, query the server for the current database
    let db_name = if !parsed.database_was_explicit {
        // create_client(config) calls `with_database(config.database)` when we're not sure which DB is the real default
        let client = Client::default()
            .with_url(format!(
                "{}://{}:{}",
                if config.use_ssl { "https" } else { "http" },
                config.host,
                config.host_port
            ))
            .with_user(config.user.to_string())
            .with_password(config.password.to_string());

        // No database was specified in URL, query the server
        client
            .query("select database()")
            .fetch_one::<String>()
            .await
            .map_err(|e| {
                RoutineFailure::new(
                    Message::new("Failure".to_string(), "fetching database".to_string()),
                    e,
                )
            })?
    } else {
        config.db_name.clone()
    };

    // Update config with detected database name if it changed
    if db_name != config.db_name {
        config.db_name = db_name.clone();
    }

    Ok((create_readonly_client(config), db_name))
}

fn write_external_models_file(
    language: SupportedLanguages,
    tables: &[Table],
    file_path: Option<&str>,
    source_dir: &str,
) -> Result<(), RoutineFailure> {
    let file = match (language, file_path) {
        (_, Some(path)) => Cow::Borrowed(path),
        (SupportedLanguages::Typescript, None) => {
            Cow::Owned(format!("{source_dir}/{TYPESCRIPT_EXTERNAL_FILE}"))
        }
    };
    match language {
        SupportedLanguages::Typescript => {
            let table_definitions =
                tables_to_typescript(tables, Some(LifeCycle::ExternallyManaged));
            let header = "// AUTO-GENERATED FILE. DO NOT EDIT.\n// This file will be replaced when you run `typed-clickhouse db pull`.";
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&*file)
                .map_err(|e| {
                    RoutineFailure::new(
                        Message::new("Failure".to_string(), format!("opening {file}")),
                        e,
                    )
                })?;
            writeln!(file, "{}\n\n{}", header, table_definitions).map_err(|e| {
                RoutineFailure::new(
                    Message::new(
                        "Failure".to_string(),
                        "writing externally managed table definitions".to_string(),
                    ),
                    e,
                )
            })?
        }
    }

    Ok(())
}

/// Pulls schema for ExternallyManaged tables and regenerates only external model files.
/// Does not modify `main.py` or `index.ts`.
pub async fn db_pull(
    remote_url: &str,
    project: &Project,
    file_path: Option<&str>,
) -> Result<(), RoutineFailure> {
    let (client, db) = create_client_and_db(remote_url).await?;
    db_pull_with_client(client, &db, project, file_path).await
}

/// Pulls schema for ExternallyManaged tables using a ClickHouseRemote struct directly.
///
/// This avoids the URL-to-struct conversion and allows using credentials resolved
/// from `[dev.remote_clickhouse]` config with keychain credentials.
pub async fn db_pull_from_remote(
    remote: &ClickHouseRemote,
    project: &Project,
    file_path: Option<&str>,
) -> Result<(), RoutineFailure> {
    let (client, db) = remote.build_client();
    db_pull_with_client(client, &db, project, file_path).await
}

/// Shared implementation for db pull operations.
///
/// Introspects the remote ClickHouse, finds external/unknown tables,
/// and regenerates the external models file.
async fn db_pull_with_client(
    client: ConfiguredDBClient,
    db: &str,
    project: &Project,
    file_path: Option<&str>,
) -> Result<(), RoutineFailure> {
    show_message!(
        MessageType::Info,
        Message {
            action: "Connecting".to_string(),
            details: "to remote ClickHouse...".to_string(),
        }
    );

    debug!("Loading InfrastructureMap from user code (DMV2)");
    // Don't resolve credentials for code generation - only needs structure
    let infra_map = InfrastructureMap::load_from_user_code(project, false)
        .await
        .map_err(|e| {
            RoutineFailure::error(Message::new(
                "Failure".to_string(),
                format!("loading infra map: {e:?}"),
            ))
        })?;

    let externally_managed_names: std::collections::HashSet<String> = infra_map
        .tables
        .values()
        .filter(|t| t.life_cycle == LifeCycle::ExternallyManaged)
        .map(|t| t.name.clone())
        .collect();

    // Names of all known tables in the project (managed or external)
    let known_table_names: std::collections::HashSet<String> =
        infra_map.tables.values().map(|t| t.name.clone()).collect();

    show_message!(
        MessageType::Info,
        Message {
            action: "Introspecting".to_string(),
            details: "remote tables...".to_string(),
        }
    );
    let (tables, _unsupported) = client.list_tables(db, project).await.map_err(|e| {
        RoutineFailure::new(
            Message::new("Failure".to_string(), "listing tables".to_string()),
            e,
        )
    })?;

    // Overwrite the external models file with:
    // - existing external tables (from infra map)
    // - plus any unknown (not present in infra map) tables, marked as external
    // Clear remote database name so generated code uses the local default
    let mut tables_for_external_file: Vec<Table> = tables
        .into_iter()
        .filter(|t| {
            externally_managed_names.contains(&t.name) || !known_table_names.contains(&t.name)
        })
        .map(|mut t| {
            t.database = None;
            t
        })
        .collect();

    // Keep a stable ordering for deterministic output
    tables_for_external_file.sort_by(|a, b| a.name.cmp(&b.name));

    write_external_models_file(
        project.language,
        &tables_for_external_file,
        file_path,
        &project.source_dir,
    )?;

    show_message!(
        MessageType::Info,
        Message {
            action: "External models".to_string(),
            details: format!("refreshed ({} table(s))", tables_for_external_file.len()),
        }
    );

    Ok(())
}
