use std::{collections::HashMap, path::Path};

use crate::utilities::constants::PACKAGE_JSON;
use crate::{framework::versions::Version, utilities::constants::TYPESCRIPT_MAIN_FILE};
use config::{Config, ConfigError, File};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TypescriptProject {
    pub name: String,
    pub version: Version,
    pub scripts: HashMap<String, String>,
    pub dependencies: HashMap<String, String>,
    pub dev_dependencies: HashMap<String, String>,
    /// Preserve any additional fields from package.json (e.g. pnpm, npm, yarn config)
    #[serde(flatten)]
    pub additional_fields: HashMap<String, serde_json::Value>,
}

impl Default for TypescriptProject {
    fn default() -> Self {
        Self {
            name: "new_project".to_string(),
            version: Version::from_string("0.0".to_string()),
            // For local development of the CLI,
            // change `typed-clickhouse` to `<REPO_PATH>/target/debug/typed-clickhouse`
            scripts: HashMap::from([
                ("tch".to_string(), "typed-clickhouse".to_string()),
                ("build".to_string(), "typed-clickhouse build".to_string()),
            ]),
            dependencies: HashMap::from([
                ("typescript".to_string(), "^5.7.0".to_string()),
                ("@typed-clickhouse/core".to_string(), "latest".to_string()),
                ("ts-patch".to_string(), "^3.3.0".to_string()),
                ("typia".to_string(), "^7.6.0".to_string()),
            ]),
            dev_dependencies: HashMap::from([
                ("@typed-clickhouse/cli".to_string(), "latest".to_string()),
                ("@types/node".to_string(), "^20.12.12".to_string()),
            ]),
            additional_fields: HashMap::new(),
        }
    }
}

impl TypescriptProject {
    pub fn load(directory: &Path) -> Result<Self, ConfigError> {
        let mut package_json_location = directory.to_path_buf();
        package_json_location.push(PACKAGE_JSON);

        Config::builder()
            .add_source(File::from(package_json_location).required(true))
            .build()?
            .try_deserialize()
    }

    pub fn main_file(&self) -> &str {
        TYPESCRIPT_MAIN_FILE
    }
}
