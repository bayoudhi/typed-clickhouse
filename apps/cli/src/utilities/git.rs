use serde::{Deserialize, Serialize};

fn default_branch() -> String {
    "main".to_string()
}

/// Git settings read from `tch.config.toml`.
///
/// The repository-manipulating helpers that used to live here (`is_git_repo`,
/// `create_init_commit`, `create_code_generation_commit`) existed only to
/// scaffold and commit into a project created by `typed-clickhouse init`. That command was
/// removed, so only the configuration type remains.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GitConfig {
    #[serde(default = "default_branch")]
    pub main_branch_name: String,
}

impl Default for GitConfig {
    fn default() -> GitConfig {
        GitConfig {
            main_branch_name: default_branch(),
        }
    }
}
