//! # Project
//!
//! This module contains the `Project` struct, which represents a users project.
//! These projects are data-intensive applications or services.
//! A project is defined by a `tch.config.toml` file and is stored
//!  in the `$PROJECT_PATH/.tch` directory.
//!
//! ## Configuration Loading
//!
//! Project configuration is loaded in the following order (later sources override earlier ones):
//! 1. **Default values** in code
//! 2. **`tch.config.toml`** (or legacy `moose.config.toml`)
//! 3. **`.env`** - Base environment variables (committed to git)
//! 4. **`.env.{environment}`** - Environment-specific variables (e.g., `.env.development`, `.env.production`)
//! 5. **`.env.local`** - Local overrides (gitignored, for developer secrets)
//! 6. **System environment variables** with `TCH_` prefix (highest priority)
//!
//! ### Environment Variable Format
//! Environment variables use the `TCH_` prefix with double underscores for nesting:
//! - `TCH_CLICKHOUSE_CONFIG__URL` → `clickhouse_config.url`
//! - `TCH_FEATURES__OLAP=true` → `features.olap`
//!
//! ### Environment Detection
//! The environment is automatically determined from the CLI command:
//! - `typed-clickhouse build` → production, loads `.env.production`
//! - every other command → development, loads `.env.development`
//!
//! ## Infrastructure Loading (`load_infra` flag)
//! - The `load_infra` flag in `tch.config.toml` determines whether this instance should load infrastructure (Docker) containers.
//! - If `load_infra` is **missing** from the config, the default is **true** (infra is loaded, for backward compatibility).
//! - If `load_infra` is **present and set to true**, infra containers are loaded.
//! - If `load_infra` is **present and set to false**, infra containers are NOT loaded.
//! - If the config file is missing or malformed, infra is loaded by default.
//!
//! Example:
//! ```toml
//! load_infra = true  # or false
//! ```
//!
//! The `Project` struct contains the following fields:
//! - `name` - The name of the project
//! - `language` - The language of the project
//! - `project_file_location` - The location of the project file on disk
//! ```

use std::collections::HashMap;
pub mod typescript_project;

use std::fmt::Debug;
use std::path::PathBuf;

use crate::framework::languages::SupportedLanguages;
use crate::framework::versions::Version;
use crate::infrastructure::olap::clickhouse::config::ClickHouseConfig;
use crate::infrastructure::olap::clickhouse::IgnorableOperation;

use crate::cli::display::Message;
use crate::cli::routines::RoutineFailure;
use crate::project::typescript_project::TypescriptProject;
use crate::utilities::_true;
use crate::utilities::constants::CLI_INTERNAL_VERSIONS_DIR;
use crate::utilities::constants::ENVIRONMENT_VARIABLE_PREFIX;
use crate::utilities::constants::OLD_PROJECT_CONFIG_FILE;
use crate::utilities::constants::PROJECT_CONFIG_FILE;
use crate::utilities::constants::{APP_DIR, CLI_PROJECT_INTERNAL_DIR, SCHEMAS_DIR};
use crate::utilities::git::GitConfig;
use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use serde::Serialize;
use tracing::{debug, error};

/// Represents errors that can occur during project file operations
#[derive(Debug, thiserror::Error)]
#[error("Failed to create or delete project files")]
#[non_exhaustive]
pub enum ProjectFileError {
    /// Error when creating the internal directory structure
    InternalDirCreationFailed(std::io::Error),
    /// Generic error with custom message
    #[error("Failed to create project files: {message}")]
    Other { message: String },
    /// Standard IO error
    IO(#[from] std::io::Error),
    /// JSON serialization error
    JSONSerde(#[from] serde_json::Error),
    /// TOML serialization error
    TOMLSerde(#[from] toml::ser::Error),
}

/// Configuration for JWT authentication
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct JwtConfig {
    /// Whether to enforce JWT on all consumption APIs
    #[serde(default)]
    pub enforce_on_all_consumptions_apis: bool,
    /// Whether to enforce JWT on all ingestion APIs
    #[serde(default)]
    pub enforce_on_all_ingest_apis: bool,
    /// Secret key for JWT signing
    pub secret: String,
    /// JWT issuer
    pub issuer: String,
    /// JWT audience
    pub audience: String,
}

/// Language-specific project configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum LanguageProjectConfig {
    /// TypeScript project configuration
    Typescript(TypescriptProject),
}

impl Default for LanguageProjectConfig {
    fn default() -> Self {
        LanguageProjectConfig::Typescript(TypescriptProject::default())
    }
}

/// Authentication configuration for the project
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct AuthenticationConfig {
    /// Optional admin API key for authentication
    #[serde(default)]
    pub admin_api_key: Option<String>,
}

/// TypeScript-specific configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TypescriptConfig {
    /// Package manager to use (npm, pnpm, yarn)
    #[serde(default = "default_package_manager")]
    pub package_manager: String,
}

impl Default for TypescriptConfig {
    fn default() -> Self {
        Self {
            package_manager: default_package_manager(),
        }
    }
}

fn default_package_manager() -> String {
    "npm".to_string()
}

fn default_state_storage() -> String {
    "clickhouse".to_string()
}

/// State storage configuration
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StateConfig {
    /// Storage backend. The only supported value is "clickhouse", which stores
    /// state in the ClickHouse `_MOOSE_STATE` table.
    #[serde(default = "default_state_storage")]
    pub storage: String,
}

impl Default for StateConfig {
    fn default() -> Self {
        StateConfig {
            storage: default_state_storage(),
        }
    }
}

/// Feature flags for the project
///
/// Streaming (Kafka/Redpanda), workflows (Temporal) and the analytics APIs
/// server were removed from this tool; their flags were removed from this
/// struct too. Deserialisation does not use `deny_unknown_fields`, so a
/// `tch.config.toml` (or legacy `moose.config.toml`) carrying a stale
/// `streaming_engine`, `workflows` or `apis` key under `[features]` still
/// loads — the unknown key is silently ignored rather than failing.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ProjectFeatures {
    /// Whether OLAP (ClickHouse) is enabled
    #[serde(default = "_true")]
    pub olap: bool,
}

impl Default for ProjectFeatures {
    fn default() -> Self {
        ProjectFeatures { olap: true }
    }
}

/// Migration configuration
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct MigrationConfig {
    /// Operations to ignore during migration plan generation
    #[serde(default)]
    pub ignore_operations: Vec<IgnorableOperation>,
}

/// Configuration for development mode behavior with externally managed tables
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct DevExternallyManagedConfig {
    /// Create local mirror tables for EXTERNALLY_MANAGED tables in dev
    #[serde(default)]
    pub create_local_mirrors: bool,

    /// Number of sample rows to seed (0 = schema only, no data)
    #[serde(default)]
    pub sample_size: usize,

    /// Refresh mirrors on every startup (vs. only if missing)
    #[serde(default)]
    pub refresh_on_startup: bool,
}

/// Configuration for externally managed tables in development mode
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct DevExternallyManagedTablesConfig {
    /// Settings for creating local mirror tables from externally managed tables
    #[serde(default)]
    pub tables: DevExternallyManagedConfig,
}

/// Protocol for remote ClickHouse connections
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClickHouseProtocol {
    /// HTTP/HTTPS protocol (default)
    #[default]
    Http,
    // Native protocol will be added later
}

/// Remote ClickHouse connection config (no credentials - stored in keychain)
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct RemoteClickHouseConfig {
    /// Connection protocol (default: HTTP)
    #[serde(default)]
    pub protocol: ClickHouseProtocol,

    /// Remote ClickHouse host
    pub host: Option<String>,

    /// Optional port (resolved to 8443 for SSL or 8123 for non-SSL during config resolution)
    #[serde(default)]
    pub port: Option<u16>,

    /// Database name
    pub database: Option<String>,

    /// Use SSL/TLS (default: true)
    #[serde(default = "_true")]
    pub use_ssl: bool,
}

impl Default for RemoteClickHouseConfig {
    fn default() -> Self {
        Self {
            protocol: ClickHouseProtocol::default(),
            host: None,
            port: None,
            database: None,
            use_ssl: true,
        }
    }
}

impl RemoteClickHouseConfig {
    /// Returns the effective port, falling back to 8443 for SSL or 8123 for non-SSL.
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(if self.use_ssl { 8443 } else { 8123 })
    }
}

/// Development mode configuration
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct DevConfig {
    /// Configuration for externally managed tables
    #[serde(default)]
    pub externally_managed: DevExternallyManagedTablesConfig,

    /// Main read-only remote ClickHouse connection (e.g., production)
    /// No credentials stored - they go in OS keychain or env vars
    #[serde(default)]
    pub remote_clickhouse: Option<RemoteClickHouseConfig>,
}

/// Represents a project
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Project {
    /// Programming language used in the project
    pub language: SupportedLanguages,
    /// Custom source directory path (defaults to "app")
    #[serde(default = "default_source_dir")]
    pub source_dir: String,
    /// ClickHouse database configuration
    pub clickhouse_config: ClickHouseConfig,
    /// Git configuration
    #[serde(default)]
    pub git_config: GitConfig,
    /// State storage configuration
    #[serde(default)]
    pub state_config: StateConfig,
    /// Migration configuration
    #[serde(default)]
    pub migration_config: MigrationConfig,
    /// Language-specific project configuration (not serialized)
    #[serde(skip)]
    pub language_project_config: LanguageProjectConfig,
    /// Project root directory location (not serialized)
    #[serde(skip)]
    pub project_location: PathBuf,
    /// Whether the project is running in production mode
    #[serde(skip, default = "Project::default_production")]
    pub is_production: bool,
    /// Whether to log payloads for debugging (not serialized, set at runtime)
    #[serde(skip)]
    pub log_payloads: bool,
    /// Map of supported old versions and their locations
    #[serde(default = "HashMap::new")]
    pub supported_old_versions: HashMap<Version, String>,
    /// JWT configuration
    #[serde(default)]
    pub jwt: Option<JwtConfig>,
    /// Authentication configuration
    #[serde(default)]
    pub authentication: AuthenticationConfig,

    /// Feature flags
    #[serde(default)]
    pub features: ProjectFeatures,
    /// Whether this instance should load infra containers (see module docs)
    #[serde(default)]
    pub load_infra: Option<bool>,
    /// TypeScript-specific configuration
    #[serde(default)]
    pub typescript_config: TypescriptConfig,
    /// Development mode configuration
    #[serde(default)]
    pub dev: DevConfig,
}

pub fn default_source_dir() -> String {
    APP_DIR.to_string()
}

impl Project {
    /// Returns the default production state (false)
    pub fn default_production() -> bool {
        false
    }

    /// Returns the project name based on the language configuration
    pub fn name(&self) -> String {
        match &self.language_project_config {
            LanguageProjectConfig::Typescript(p) => p.name.clone(),
        }
    }

    pub fn main_file(&self) -> PathBuf {
        let mut location = self.app_dir();
        location.push(match &self.language_project_config {
            LanguageProjectConfig::Typescript(p) => p.main_file(),
        });
        location
    }

    /// Loads a project from the specified directory
    ///
    /// # Arguments
    ///
    /// * `directory` - The project directory containing tch.config.toml and .env files
    /// * `environment` - The runtime environment (development or production)
    ///
    /// # Configuration Loading Order
    ///
    /// 1. Load .env files (.env → .env.{dev|prod} → .env.local for dev only)
    /// 2. Load tch.config.toml (falling back to moose.config.toml)
    /// 3. Apply TCH_* environment variable overrides
    pub fn load(
        directory: &PathBuf,
        environment: crate::utilities::dotenv::RuntimeEnvironment,
    ) -> Result<Project, ConfigError> {
        // 1. Load .env files first (this populates environment variables)
        crate::utilities::dotenv::load_dotenv_files(directory, environment);

        let mut project_file = directory.clone();

        // 2. Prioritize the new project file name
        if directory.clone().join(PROJECT_CONFIG_FILE).exists() {
            project_file.push(PROJECT_CONFIG_FILE);
        } else {
            project_file.push(OLD_PROJECT_CONFIG_FILE);
        }

        // 3. Build config with TOML file + environment variables
        let mut project_config: Project = Config::builder()
            .add_source(File::from(project_file).required(true))
            .add_source(
                Environment::with_prefix(ENVIRONMENT_VARIABLE_PREFIX)
                    .prefix_separator("_")
                    .separator("__"),
            )
            .build()?
            .try_deserialize()?;

        project_config.project_location.clone_from(directory);

        match project_config.language {
            SupportedLanguages::Typescript => {
                let ts_config = TypescriptProject::load(directory)?;
                project_config.language_project_config =
                    LanguageProjectConfig::Typescript(ts_config);
            }
        }

        Ok(project_config)
    }

    /// Loads a project from the current directory with the specified environment
    ///
    /// # Arguments
    ///
    /// * `environment` - The runtime environment (development or production)
    pub fn load_from_current_dir(
        environment: crate::utilities::dotenv::RuntimeEnvironment,
    ) -> Result<Project, ConfigError> {
        let current_dir = std::env::current_dir().expect("Failed to get the current directory");
        Project::load(&current_dir, environment)
    }

    /// Returns the path to the app directory
    pub fn app_dir(&self) -> PathBuf {
        let mut app_dir = self.project_location.clone();
        app_dir.push(&self.source_dir);

        debug!("App dir: {:?}", app_dir);

        if !app_dir.exists() {
            std::fs::create_dir_all(&app_dir).expect("Failed to create app directory");
        }
        app_dir
    }

    /// Returns the path to the data models directory
    pub fn data_models_dir(&self) -> PathBuf {
        let mut schemas_dir = self.app_dir();
        schemas_dir.push(SCHEMAS_DIR);

        if !schemas_dir.exists() {
            std::fs::create_dir_all(&schemas_dir).expect("Failed to create schemas directory");
        }

        debug!("Schemas dir: {:?}", schemas_dir);
        schemas_dir
    }

    /// Returns the path to the versioned data model directory
    pub fn versioned_data_model_dir(&self, version: &str) -> Result<PathBuf, ProjectFileError> {
        if version == self.cur_version().as_str() {
            Ok(self.data_models_dir())
        } else {
            Ok(self.old_version_location(version)?)
        }
    }

    /// Returns the path to the internal directory
    pub fn internal_dir(&self) -> Result<PathBuf, ProjectFileError> {
        let mut internal_dir = self.project_location.clone();
        internal_dir.push(CLI_PROJECT_INTERNAL_DIR);

        if !internal_dir.is_dir() {
            if internal_dir.exists() {
                debug!("Internal dir exists as a file: {:?}", internal_dir);
                return Err(ProjectFileError::Other {
                    message: format!(
                        "The {CLI_PROJECT_INTERNAL_DIR} file exists but is not a directory"
                    ),
                });
            } else {
                debug!("Creating internal dir: {:?}", internal_dir);
                std::fs::create_dir_all(&internal_dir).map_err(|e| ProjectFileError::Other {
                    message: format!(
                        "Failed to create internal directory {}: {}",
                        internal_dir.display(),
                        e
                    ),
                })?;
            }
        } else {
            debug!("Internal directory Exists: {:?}", internal_dir);
        }

        Ok(internal_dir)
    }

    pub fn internal_dir_with_routine_failure_err(&self) -> Result<PathBuf, RoutineFailure> {
        self.internal_dir().map_err(|err| {
            error!("Failed to get internal directory for project: {}", err);
            RoutineFailure::new(
                Message::new(
                    "Failed".to_string(),
                    "to get internal directory for project".to_string(),
                ),
                err,
            )
        })
    }

    /// Deletes the internal directory
    pub fn delete_internal_dir(&self) -> Result<(), ProjectFileError> {
        let internal_dir = self.internal_dir()?;
        Ok(std::fs::remove_dir_all(internal_dir)?)
    }

    /// Returns the location of an old version
    pub fn old_version_location(&self, version: &str) -> Result<PathBuf, ProjectFileError> {
        let mut old_base_path = self.internal_dir()?;
        old_base_path.push(CLI_INTERNAL_VERSIONS_DIR);
        old_base_path.push(version);

        Ok(old_base_path)
    }

    /// Returns the current version
    pub fn cur_version(&self) -> &Version {
        match &self.language_project_config {
            LanguageProjectConfig::Typescript(package_json) => &package_json.version,
        }
    }

    /// Returns all versions including current
    pub fn versions(&self) -> Vec<String> {
        vec![self.cur_version().to_string()]
    }

    /// Checks if the project is running in a docker container
    pub fn is_docker_image(&self) -> bool {
        std::env::var("DOCKER_IMAGE").unwrap_or("false".to_string()) == "true"
    }

    /// Returns true if this instance should load infra containers, according to the load_infra flag.
    ///
    /// - If load_infra is Some(true) or None (missing), returns true (default: load infra).
    /// - If load_infra is Some(false), returns false (do not load infra).
    pub fn should_load_infra(&self) -> bool {
        self.load_infra.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `[features]` block from a project created before streaming, workflows
    /// and the analytics APIs server were removed still has those keys. This
    /// goes through the same `config::Config::builder()` + `try_deserialize()`
    /// path as `Project::load` to prove such a file keeps loading instead of
    /// erroring on the now-unknown keys.
    #[test]
    fn stale_feature_flags_are_ignored_not_fatal() {
        let toml = r#"
            streaming_engine = true
            workflows = true
            apis = true
            olap = false
        "#;

        let features: ProjectFeatures = Config::builder()
            .add_source(File::from_str(toml, config::FileFormat::Toml))
            .build()
            .expect("stale keys should not prevent building the config")
            .try_deserialize()
            .expect("stale keys should be ignored, not fatal");

        assert!(!features.olap, "the only surviving key should still load");
    }
}
