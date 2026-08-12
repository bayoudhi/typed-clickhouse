//! Settings module for the CLI
//!
//! This module handles configuration management for the CLI, including:
//! - Reading/writing configuration from the user's home directory
//! - Environment variable overrides
//! - Default configuration values
//!
//! # Configuration Sources
//! Configuration is loaded in the following order (later sources override earlier ones):
//! 1. Default values
//! 2. Local configuration file (~/.tch/config.toml)
//! 3. Environment variables (prefixed with TCH_)
//!
//! # Environment Variables
//! Environment variables can override any config value using double underscores as separators:
//! - `TCH_LOGGER__LEVEL=debug`

use config::{Config, ConfigError, Environment, File};
use home::home_dir;
use serde::Deserialize;
use std::path::PathBuf;
use toml_edit::{table, value, DocumentMut, Entry, Item};
use tracing::warn;

use super::display::{Message, MessageType};
use super::logger::LoggerSettings;
use crate::utilities::constants::{
    CLI_CONFIG_FILE, CLI_USER_DIRECTORY, ENVIRONMENT_VARIABLE_PREFIX,
};

/// Main settings structure containing all configuration options
#[derive(Deserialize, Debug, Clone)]
pub struct Settings {
    /// Logging configuration settings
    #[serde(default)]
    pub logger: LoggerSettings,
}

/// Returns the path to the config file in the user's home directory
fn config_path() -> Result<PathBuf, std::io::Error> {
    let mut path: PathBuf = user_directory()?;
    path.push(CLI_CONFIG_FILE);
    Ok(path)
}

/// Returns the path to the CLI user directory
pub fn user_directory() -> Result<PathBuf, std::io::Error> {
    let mut path: PathBuf = home_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory. Ensure HOME is set.",
        )
    })?;
    path.push(CLI_USER_DIRECTORY);
    Ok(path)
}

/// Creates the CLI user directory if it doesn't exist
pub fn setup_user_directory() -> Result<(), std::io::Error> {
    let path = user_directory()?;
    std::fs::create_dir_all(path)?;
    Ok(())
}

/// Reads and parses the settings from all configuration sources
///
/// Configuration is loaded in the following order:
/// 1. Default values
/// 2. Local configuration file
/// 3. Environment variables (prefixed with TCH_)
///
/// Returns a Result containing the parsed Settings or a ConfigError
pub fn read_settings() -> Result<Settings, ConfigError> {
    let config_file_location: PathBuf =
        config_path().map_err(|e| ConfigError::Message(e.to_string()))?;

    let s = Config::builder()
        .add_source(File::from(config_file_location).required(false))
        .add_source(
            Environment::with_prefix(ENVIRONMENT_VARIABLE_PREFIX)
                .try_parsing(true)
                .prefix_separator("_")
                .separator("__"),
        )
        .build()?;

    let settings: Settings = s.try_deserialize()?;

    Ok(settings)
}

/// Initializes the config file with default values if it doesn't exist
///
/// If the config file already exists, this function will:
/// 1. Parse the existing TOML
/// 2. Ensure required fields are present
/// 3. Add any missing fields with default values
/// 4. Write the updated config back to disk
///
/// Returns a Result indicating success or an IO error
pub fn init_config_file() -> Result<(), std::io::Error> {
    let path = config_path()?;
    if !path.exists() {
        let contents_toml = r#"
[telemetry]
is_developer=false
"#;
        if let Err(e) = std::fs::write(&path, contents_toml) {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                eprintln!(
                    "Config file {} could not be created (read-only or externally managed); using defaults",
                    path.display()
                );
                return Ok(());
            }
            return Err(e);
        }
    } else {
        let data = std::fs::read_to_string(&path)?;
        match data.parse::<DocumentMut>() {
            Ok(mut toml) => {
                let table = match toml.get_mut("telemetry") {
                    Some(Item::Table(table)) => table,
                    Some(_) => {
                        warn!("telemetry in config is not a table.");
                        return Ok(());
                    }
                    None => {
                        toml["telemetry"] = table();
                        toml["telemetry"].as_table_mut().unwrap()
                    }
                };

                let mut changed = false;
                if let Entry::Vacant(e) = table.entry("is_developer") {
                    e.insert(value(false));
                    changed = true;
                }

                if changed {
                    if let Err(e) = std::fs::write(&path, toml.to_string()) {
                        if e.kind() == std::io::ErrorKind::PermissionDenied {
                            warn!(
                                "Config file {} is read-only (externally managed); skipping write",
                                path.display()
                            );
                            return Ok(());
                        }
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                show_message!(
                    MessageType::Error,
                    Message {
                        action: "Init".to_string(),
                        details: format!("Error parsing config file: {e:?}"),
                    }
                );
            }
        }
    }
    Ok(())
}
