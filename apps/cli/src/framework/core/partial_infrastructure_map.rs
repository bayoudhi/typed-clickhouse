//! Partial Infrastructure Map Module
//!
//! This module provides functionality for loading and converting infrastructure definitions from user code
//! into a complete infrastructure map. It serves as a bridge between user-defined infrastructure specifications
//! (typically written in TypeScript or Python) and the internal Rust representation used by the framework.
//!
//! # Key Components
//!
//! * [`PartialInfrastructureMap`] - The main structure that represents a partially defined infrastructure
//! * [`PartialTable`] - Component for the table infrastructure element
//! * [`DmV2LoadingError`] - Error type for handling failures during infrastructure loading
//!
//! # Usage
//!
//! The module is primarily used during the framework's initialization phase to:
//! 1. Load infrastructure definitions from user code
//! 2. Validate and transform these definitions
//! 3. Create a complete infrastructure map for the framework to use
//!
//! # Example
//!
//! ```no_run
//! use framework_cli::framework::core::partial_infrastructure_map::PartialInfrastructureMap;
//! use tokio::process::Child;
//! use std::path::Path;
//!
//! async fn load_infrastructure(process: Child, file_name: &str) -> Result<(), DmV2LoadingError> {
//!     let partial_map = PartialInfrastructureMap::from_subprocess(process, file_name).await?;
//!     let complete_map = partial_map.into_infra_map(
//!         SupportedLanguages::TypeScript,
//!         Path::new("main.ts")
//!     )?;
//!     Ok(())
//! }
//! ```

use std::{collections::HashMap, path::Path};

use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tracing::debug;

use super::{
    infrastructure::{
        sql_resource::SqlResource,
        table::{Column, Metadata, Table, TableIndex},
        view::Dmv1View,
    },
    infrastructure_map::{InfrastructureMap, PrimitiveSignature, PrimitiveTypes},
};
use crate::framework::core::infrastructure::table::{OrderBy, SeedFilter, TableProjection};
use crate::infrastructure::olap::clickhouse::queries::BufferEngine;
use crate::{
    framework::versions::Version,
    infrastructure::olap::clickhouse::queries::ClickhouseEngine,
    utilities::{constants, normalize_path_string},
};

/// Defines how the CLI manages the lifecycle of database resources when code changes.
///
/// This enum controls the behavior when there are differences between code definitions
/// and the actual database schema or structure.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LifeCycle {
    /// Full automatic management (default behavior).
    /// The CLI will automatically modify database resources to match code definitions,
    /// including potentially destructive operations like dropping columns or tables.
    #[default]
    FullyManaged,

    /// Deletion-protected automatic management.
    /// The CLI will modify resources to match code but will avoid destructive actions
    /// such as dropping columns or tables. Only additive changes are applied.
    DeletionProtected,

    /// External management - no automatic changes.
    /// The CLI will not modify the database resources. You are responsible for managing
    /// the schema and ensuring it matches code definitions manually.
    ExternallyManaged,
}

impl LifeCycle {
    pub fn default_for_deserialization() -> LifeCycle {
        LifeCycle::default()
    }

    /// Returns true if this lifecycle protects the table from being dropped.
    ///
    /// Protected lifecycles: `DeletionProtected`, `ExternallyManaged`
    #[inline]
    pub fn is_drop_protected(self) -> bool {
        matches!(self, Self::DeletionProtected | Self::ExternallyManaged)
    }

    /// Returns true if this lifecycle protects columns from being removed.
    ///
    /// Protected lifecycles: `DeletionProtected`, `ExternallyManaged`
    #[inline]
    pub fn is_column_removal_protected(self) -> bool {
        matches!(self, Self::DeletionProtected | Self::ExternallyManaged)
    }

    /// Returns true if this lifecycle protects the table from ANY modifications.
    ///
    /// When true, The CLI should not attempt to change the table in any way -
    /// no column additions, no column removals, no TTL changes, no settings changes.
    /// The table is managed externally and the CLI only reads from it.
    ///
    /// Protected lifecycles: `ExternallyManaged`
    #[inline]
    pub fn is_any_modification_protected(self) -> bool {
        self == Self::ExternallyManaged
    }
}

/// Represents a table definition from user code before it's converted into a complete [`Table`].
///
/// This structure captures the essential properties needed to create a table in the infrastructure,
/// including column definitions, ordering, and deduplication settings.
/// Engine-specific configuration using discriminated union pattern.
/// This provides type-safe deserialization of engine configurations from TypeScript.

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct S3QueueConfig {
    s3_path: String,
    format: String,
    aws_access_key_id: Option<String>,
    aws_secret_access_key: Option<String>,
    compression: Option<String>,
    headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct S3Config {
    path: String,
    format: String,
    aws_access_key_id: Option<String>,
    aws_secret_access_key: Option<String>,
    compression: Option<String>,
    partition_strategy: Option<String>,
    partition_columns_in_data_file: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BufferConfig {
    target_database: String,
    target_table: String,
    num_layers: u32,
    min_time: u32,
    max_time: u32,
    min_rows: u64,
    max_rows: u64,
    min_bytes: u64,
    max_bytes: u64,
    flush_time: Option<u32>,
    flush_rows: Option<u64>,
    flush_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DistributedConfig {
    cluster: String,
    target_database: String,
    target_table: String,
    sharding_key: Option<String>,
    policy_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IcebergS3Config {
    path: String,
    format: String,
    aws_access_key_id: Option<String>,
    aws_secret_access_key: Option<String>,
    compression: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct KafkaConfig {
    broker_list: String,
    topic_list: String,
    group_name: String,
    format: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "engine", rename_all = "camelCase")]
enum EngineConfig {
    #[serde(rename = "MergeTree")]
    MergeTree {},

    #[serde(rename = "ReplacingMergeTree")]
    ReplacingMergeTree {
        #[serde(default)]
        ver: Option<String>,
        #[serde(alias = "isDeleted", default)]
        is_deleted: Option<String>,
    },

    #[serde(rename = "AggregatingMergeTree")]
    AggregatingMergeTree {},

    #[serde(rename = "SummingMergeTree")]
    SummingMergeTree {
        #[serde(default)]
        columns: Option<Vec<String>>,
    },

    #[serde(rename = "CollapsingMergeTree")]
    CollapsingMergeTree { sign: String },

    #[serde(rename = "VersionedCollapsingMergeTree")]
    VersionedCollapsingMergeTree { sign: String, ver: String },

    #[serde(rename = "ReplicatedMergeTree")]
    ReplicatedMergeTree {
        #[serde(alias = "keeperPath", default)]
        keeper_path: Option<String>,
        #[serde(alias = "replicaName", default)]
        replica_name: Option<String>,
    },

    #[serde(rename = "ReplicatedReplacingMergeTree")]
    ReplicatedReplacingMergeTree {
        #[serde(alias = "keeperPath", default)]
        keeper_path: Option<String>,
        #[serde(alias = "replicaName", default)]
        replica_name: Option<String>,
        #[serde(default)]
        ver: Option<String>,
        #[serde(alias = "isDeleted", default)]
        is_deleted: Option<String>,
    },

    #[serde(rename = "ReplicatedAggregatingMergeTree")]
    ReplicatedAggregatingMergeTree {
        #[serde(alias = "keeperPath", default)]
        keeper_path: Option<String>,
        #[serde(alias = "replicaName", default)]
        replica_name: Option<String>,
    },

    #[serde(rename = "ReplicatedSummingMergeTree")]
    ReplicatedSummingMergeTree {
        #[serde(alias = "keeperPath", default)]
        keeper_path: Option<String>,
        #[serde(alias = "replicaName", default)]
        replica_name: Option<String>,
        #[serde(default)]
        columns: Option<Vec<String>>,
    },

    #[serde(rename = "ReplicatedCollapsingMergeTree")]
    ReplicatedCollapsingMergeTree {
        #[serde(alias = "keeperPath", default)]
        keeper_path: Option<String>,
        #[serde(alias = "replicaName", default)]
        replica_name: Option<String>,
        sign: String,
    },

    #[serde(rename = "ReplicatedVersionedCollapsingMergeTree")]
    ReplicatedVersionedCollapsingMergeTree {
        #[serde(alias = "keeperPath", default)]
        keeper_path: Option<String>,
        #[serde(alias = "replicaName", default)]
        replica_name: Option<String>,
        sign: String,
        ver: String,
    },

    #[serde(rename = "S3Queue")]
    S3Queue(Box<S3QueueConfig>),

    #[serde(rename = "S3")]
    S3(Box<S3Config>),

    #[serde(rename = "Buffer")]
    Buffer(Box<BufferConfig>),

    #[serde(rename = "Distributed")]
    Distributed(Box<DistributedConfig>),

    #[serde(rename = "IcebergS3")]
    IcebergS3(Box<IcebergS3Config>),

    #[serde(rename = "Kafka")]
    Kafka(Box<KafkaConfig>),

    #[serde(rename = "Merge")]
    Merge {
        #[serde(alias = "sourceDatabase")]
        source_database: String,
        #[serde(alias = "tablesRegexp")]
        tables_regexp: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PartialTable {
    pub name: String,
    pub columns: Vec<Column>,
    #[serde(alias = "order_by")]
    pub order_by: OrderBy,
    #[serde(default)]
    pub partition_by: Option<String>,
    #[serde(default, alias = "sampleByExpression")]
    pub sample_by: Option<String>,
    #[serde(alias = "engine_config")]
    pub engine_config: Option<EngineConfig>,
    pub version: Option<String>,
    pub metadata: Option<Metadata>,
    #[serde(alias = "life_cycle")]
    pub life_cycle: Option<LifeCycle>,
    #[serde(alias = "table_settings")]
    pub table_settings: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub indexes: Vec<TableIndex>,
    #[serde(default)]
    pub projections: Vec<TableProjection>,
    /// Optional table-level TTL expression (ClickHouse expression, without leading 'TTL')
    #[serde(alias = "ttl")]
    pub ttl: Option<String>,
    /// Optional database name for multi-database support
    #[serde(default)]
    pub database: Option<String>,
    /// Optional cluster name for ON CLUSTER support
    #[serde(default)]
    pub cluster: Option<String>,
    /// Optional PRIMARY KEY expression (overrides column-level primary_key flags when specified)
    #[serde(default, alias = "primary_key_expression")]
    pub primary_key_expression: Option<String>,
    /// Per-table filter for `typed-clickhouse seed clickhouse`
    #[serde(
        default,
        alias = "seed_filter",
        deserialize_with = "crate::framework::core::infrastructure::table::deserialize_nullable_as_default"
    )]
    pub seed_filter: SeedFilter,
}

/// Errors that can occur during the loading of Data Model V2 infrastructure definitions.
///
/// This error type follows the Rust error handling best practices and provides
/// specific error variants for different failure modes.
#[derive(Debug, thiserror::Error)]
#[error("Failed to load Data Model V2")]
#[non_exhaustive]
pub enum DmV2LoadingError {
    /// Errors from Tokio async I/O operations
    Tokio(#[from] tokio::io::Error),

    /// Errors when collecting resources from user code
    #[error("Error collecting resources from {user_code_file_name}:\n{message}")]
    StdErr {
        user_code_file_name: String,
        message: String,
    },

    /// JSON parsing errors
    JsonParsing(#[from] serde_json::Error),

    /// Runtime environment variable resolution errors
    #[error("Failed to resolve runtime environment variable for table '{table_name}' field '{field}': {error}")]
    RuntimeEnvResolution {
        table_name: String,
        field: String,
        error: String,
    },

    /// Catch-all for other types of errors
    #[error("{message}")]
    Other { message: String },
}

/// Represents a partial infrastructure map loaded from user code.
///
/// This structure is the main entry point for loading and converting infrastructure
/// definitions from user code into the framework's internal representation.
///
/// # Loading Process
///
/// 1. User code is executed in a subprocess
/// 2. The subprocess outputs JSON describing the infrastructure
/// 3. The JSON is parsed into this structure
/// 4. The structure is converted into a complete [`InfrastructureMap`]
///
/// # Fields
///
/// All fields are optional HashMaps containing partial definitions for different
/// infrastructure components. During conversion to a complete map, these partial
/// definitions are validated and transformed into their complete counterparts.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartialInfrastructureMap {
    #[serde(default)]
    tables: HashMap<String, PartialTable>,
    #[serde(default)]
    dmv1_views: HashMap<String, Dmv1View>,
    #[serde(default)]
    sql_resources: HashMap<String, SqlResource>,
    #[serde(default)]
    materialized_views: HashMap<
        String,
        crate::framework::core::infrastructure::materialized_view::MaterializedView,
    >,
    #[serde(default)]
    views: HashMap<String, crate::framework::core::infrastructure::view::View>,
    #[serde(default)]
    select_row_policies:
        HashMap<String, crate::framework::core::infrastructure::select_row_policy::SelectRowPolicy>,
    /// List of source files that exist in the project but were not loaded during the build process.
    /// This is used to warn developers about potentially missing imports or configuration issues.
    /// File paths should be relative to the project root.
    #[serde(default, rename = "unloadedFiles")]
    pub unloaded_files: Vec<String>,
}

impl PartialInfrastructureMap {
    /// Creates a new [`PartialInfrastructureMap`] by executing and reading from a subprocess.
    ///
    /// This method is used to load infrastructure definitions from user code written in languages
    /// like TypeScript or Python. The subprocess is expected to output JSON in a specific format
    /// that can be parsed into a [`PartialInfrastructureMap`].
    ///
    /// # Arguments
    ///
    /// * `process` - The subprocess that will output the infrastructure definition
    /// * `user_code_file_name` - Name of the file containing the user's code
    ///
    /// # Errors
    ///
    /// Returns a [`DmV2LoadingError`] if:
    /// * The subprocess fails to execute
    /// * The subprocess output cannot be parsed
    /// * Required dependencies are missing
    /// * The output format is invalid
    pub async fn from_subprocess(
        process: Child,
        user_code_file_name: &str,
    ) -> Result<PartialInfrastructureMap, DmV2LoadingError> {
        let output = process.wait_with_output().await?;

        let raw_string_stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let raw_string_stderr = String::from_utf8_lossy(&output.stderr).to_string();

        // Try to parse stdout first. Subprocess stderr may contain non-fatal
        // warnings (e.g. Python deprecation notices) that should not block
        // resource collection when stdout carries a valid payload.
        let output_format = || DmV2LoadingError::Other {
            message: "invalid output format".to_string(),
        };

        if let Some(json) = raw_string_stdout
            .split("___TCH_INFRA_MAP___start")
            .nth(1)
            .and_then(|s| s.split("end___TCH_INFRA_MAP___").next())
        {
            if !raw_string_stderr.is_empty() {
                tracing::warn!(
                    "Subprocess for {} produced warnings on stderr:\n{}",
                    user_code_file_name,
                    raw_string_stderr,
                );
            }
            tracing::info!("load_from_user_code inframap json: {}", json);
            Ok(serde_json::from_str(json)
                .inspect_err(|_| debug!("Invalid JSON from exports: {}", raw_string_stdout))?)
        } else if !raw_string_stderr.is_empty() {
            let error_message = if raw_string_stderr.contains("MODULE_NOT_FOUND")
                || raw_string_stderr.contains("ModuleNotFoundError")
            {
                let install_command = if user_code_file_name
                    .ends_with(constants::TYPESCRIPT_FILE_EXTENSION)
                {
                    "npm install"
                } else {
                    return Err(DmV2LoadingError::Other {
                        message: format!("Unsupported file extension in: {user_code_file_name}"),
                    });
                };

                format!("Missing dependencies detected. Please run '{install_command}' and try again.\nOriginal error: {raw_string_stderr}")
            } else {
                raw_string_stderr
            };

            Err(DmV2LoadingError::StdErr {
                user_code_file_name: user_code_file_name.to_string(),
                message: error_message,
            })
        } else {
            Err(output_format())
        }
    }

    /// Converts this partial infrastructure map into a complete [`InfrastructureMap`].
    ///
    /// This method performs the final transformation of user-defined infrastructure components
    /// into their complete, validated forms. It ensures all references between components are
    /// valid and sets up the necessary processes and workers.
    ///
    /// # Arguments
    ///
    /// * `language` - The programming language of the user's code
    /// * `main_file` - Path to the main file containing the user's code
    /// * `project_root` - Root directory of the project for normalizing file paths
    ///
    /// # Returns
    ///
    /// Returns a complete [`InfrastructureMap`] containing all the validated and transformed
    /// infrastructure components.
    ///
    /// # Errors
    ///
    /// Returns a [`DmV2LoadingError`] if:
    /// * Secret resolution fails during table conversion
    /// * Engine configuration is invalid
    pub fn into_infra_map(
        self,
        default_database: &str,
        project_root: &Path,
    ) -> Result<InfrastructureMap, DmV2LoadingError> {
        let tables = self.convert_tables(default_database)?;

        let mut infra_map = InfrastructureMap {
            default_database: default_database.to_string(),
            tables,
            dmv1_views: self.dmv1_views,
            sql_resources: self.sql_resources,
            materialized_views: self.materialized_views,
            views: self.views,
            select_row_policies: self.select_row_policies,
            moose_version: None,
            data_model_version: None,
        };

        normalize_all_metadata_paths(&mut infra_map, project_root);

        Ok(infra_map)
    }

    /// Converts partial table definitions into complete [`Table`] instances.
    ///
    /// This method handles versioning and naming of tables, ensuring that versioned tables
    /// have appropriate suffixes in their names.
    ///
    /// # Errors
    ///
    /// Returns a [`DmV2LoadingError`] if:
    /// * Secret resolution fails (e.g., environment variable not found)
    /// * Engine configuration is invalid
    fn convert_tables(
        &self,
        default_database: &str,
    ) -> Result<HashMap<String, Table>, DmV2LoadingError> {
        self.tables
            .values()
            .map(|partial_table| {
                let version: Option<Version> = partial_table
                    .version
                    .as_ref()
                    .map(|v_str| Version::from_string(v_str.clone()));

                let engine = self.parse_engine(partial_table, default_database)?;
                let engine_params_hash = Some(engine.non_alterable_params_hash());

                // S3Queue settings should come directly from table_settings in the user code
                let mut table_settings = partial_table.table_settings.clone().unwrap_or_default();

                // Apply ClickHouse default settings for MergeTree family engines
                // This ensures our internal representation matches what ClickHouse actually has
                // and prevents unnecessary diffs
                let should_apply_mergetree_defaults = engine.is_merge_tree_family();

                if should_apply_mergetree_defaults {
                    // Apply MergeTree defaults if not explicitly set by user
                    // These are the most common defaults that appear in system.tables

                    // Index granularity settings (readonly after table creation)
                    table_settings
                        .entry("index_granularity".to_string())
                        .or_insert("8192".to_string());
                    table_settings
                        .entry("index_granularity_bytes".to_string())
                        .or_insert("10485760".to_string()); // 10 * 1024 * 1024

                    // In ClickHouse 19.11+, this defaults to true (readonly after creation)
                    table_settings
                        .entry("enable_mixed_granularity_parts".to_string())
                        .or_insert("1".to_string()); // true = 1 in ClickHouse settings

                    // Note: We don't set other defaults like:
                    // - min_bytes_for_wide_part (defaults to 10485760 but is modifiable)
                    // - min_rows_for_wide_part (defaults to 0 but is modifiable)
                    // - merge_max_block_size (defaults to 8192 but is modifiable)
                    // Because they are modifiable and won't cause issues if not set
                }

                // Extract table-level TTL from partial table
                let table_ttl_setting = partial_table.ttl.clone();

                // Construct the table with raw values from partial_table.
                // Canonicalization (order_by fallback, array nullability, primary_key clearing)
                // is handled by Table::canonicalize() below.
                let table = Table {
                    name: version
                        .as_ref()
                        .map_or(partial_table.name.clone(), |version| {
                            format!("{}_{}", partial_table.name, version.as_suffix())
                        }),
                    columns: partial_table.columns.clone(),
                    order_by: partial_table.order_by.clone(),
                    partition_by: partial_table.partition_by.clone(),
                    sample_by: partial_table.sample_by.clone(),
                    engine,
                    version,
                    source_primitive: PrimitiveSignature {
                        name: partial_table.name.clone(),
                        primitive_type: PrimitiveTypes::DataModel,
                    },
                    metadata: partial_table.metadata.clone(),
                    life_cycle: partial_table.life_cycle.unwrap_or(LifeCycle::FullyManaged),
                    engine_params_hash,
                    table_settings: if table_settings.is_empty() {
                        None
                    } else {
                        Some(table_settings.clone())
                    },
                    table_settings_hash: None, // Will be computed below
                    indexes: partial_table.indexes.clone(),
                    projections: partial_table.projections.clone(),
                    table_ttl_setting,
                    database: partial_table.database.clone(),
                    cluster_name: partial_table.cluster.clone(),
                    primary_key_expression: partial_table.primary_key_expression.clone(),
                    seed_filter: partial_table.seed_filter.clone(),
                };

                // Compute table_settings_hash for change detection, then canonicalize
                let mut table = table;
                table.table_settings_hash = table.compute_table_settings_hash();
                let table = table.canonicalize();

                Ok((table.id(default_database), table))
            })
            .collect()
    }

    /// Parses the engine configuration from a partial table using the discriminated union approach.
    /// This provides type-safe conversion from the serialized engine configuration to ClickhouseEngine.
    ///
    /// For S3Queue engines, this method resolves runtime environment variable markers into actual values.
    /// This ensures secrets are resolved before the infrastructure diff is calculated, allowing credential
    /// rotation to trigger table recreation.
    ///
    /// For Merge engines, `currentDatabase()` in `source_database` is resolved to the actual
    /// database name. ClickHouse resolves this function at table creation time and stores the
    /// literal name, so we must resolve it on the desired-state side to avoid false diffs.
    fn parse_engine(
        &self,
        partial_table: &PartialTable,
        default_database: &str,
    ) -> Result<ClickhouseEngine, DmV2LoadingError> {
        match &partial_table.engine_config {
            Some(EngineConfig::MergeTree {}) => Ok(ClickhouseEngine::MergeTree),

            Some(EngineConfig::ReplacingMergeTree { ver, is_deleted }) => {
                Ok(ClickhouseEngine::ReplacingMergeTree {
                    ver: ver.clone(),
                    is_deleted: is_deleted.clone(),
                })
            }

            Some(EngineConfig::AggregatingMergeTree {}) => {
                Ok(ClickhouseEngine::AggregatingMergeTree)
            }

            Some(EngineConfig::SummingMergeTree { columns }) => {
                Ok(ClickhouseEngine::SummingMergeTree {
                    columns: columns.clone(),
                })
            }

            Some(EngineConfig::CollapsingMergeTree { sign }) => {
                Ok(ClickhouseEngine::CollapsingMergeTree { sign: sign.clone() })
            }

            Some(EngineConfig::VersionedCollapsingMergeTree { sign, ver }) => {
                Ok(ClickhouseEngine::VersionedCollapsingMergeTree {
                    sign: sign.clone(),
                    version: ver.clone(),
                })
            }

            Some(EngineConfig::ReplicatedMergeTree {
                keeper_path,
                replica_name,
            }) => Ok(ClickhouseEngine::ReplicatedMergeTree {
                keeper_path: keeper_path.clone(),
                replica_name: replica_name.clone(),
            }),

            Some(EngineConfig::ReplicatedReplacingMergeTree {
                keeper_path,
                replica_name,
                ver,
                is_deleted,
            }) => Ok(ClickhouseEngine::ReplicatedReplacingMergeTree {
                keeper_path: keeper_path.clone(),
                replica_name: replica_name.clone(),
                ver: ver.clone(),
                is_deleted: is_deleted.clone(),
            }),

            Some(EngineConfig::ReplicatedAggregatingMergeTree {
                keeper_path,
                replica_name,
            }) => Ok(ClickhouseEngine::ReplicatedAggregatingMergeTree {
                keeper_path: keeper_path.clone(),
                replica_name: replica_name.clone(),
            }),

            Some(EngineConfig::ReplicatedSummingMergeTree {
                keeper_path,
                replica_name,
                columns,
            }) => Ok(ClickhouseEngine::ReplicatedSummingMergeTree {
                keeper_path: keeper_path.clone(),
                replica_name: replica_name.clone(),
                columns: columns.clone(),
            }),

            Some(EngineConfig::ReplicatedCollapsingMergeTree {
                keeper_path,
                replica_name,
                sign,
            }) => Ok(ClickhouseEngine::ReplicatedCollapsingMergeTree {
                keeper_path: keeper_path.clone(),
                replica_name: replica_name.clone(),
                sign: sign.clone(),
            }),

            Some(EngineConfig::ReplicatedVersionedCollapsingMergeTree {
                keeper_path,
                replica_name,
                sign,
                ver,
            }) => Ok(ClickhouseEngine::ReplicatedVersionedCollapsingMergeTree {
                keeper_path: keeper_path.clone(),
                replica_name: replica_name.clone(),
                sign: sign.clone(),
                version: ver.clone(),
            }),

            Some(EngineConfig::S3Queue(config)) => {
                // Keep environment variable markers as-is - credentials will be resolved at runtime
                // S3Queue settings are handled in table_settings, not in the engine
                Ok(ClickhouseEngine::S3Queue {
                    s3_path: config.s3_path.clone(),
                    format: config.format.clone(),
                    compression: config.compression.clone(),
                    headers: config.headers.clone(),
                    aws_access_key_id: config.aws_access_key_id.clone(),
                    aws_secret_access_key: config.aws_secret_access_key.clone(),
                })
            }

            Some(EngineConfig::S3(config)) => {
                // Keep environment variable markers as-is - credentials will be resolved at runtime
                Ok(ClickhouseEngine::S3 {
                    path: config.path.clone(),
                    format: config.format.clone(),
                    aws_access_key_id: config.aws_access_key_id.clone(),
                    aws_secret_access_key: config.aws_secret_access_key.clone(),
                    compression: config.compression.clone(),
                    partition_strategy: config.partition_strategy.clone(),
                    partition_columns_in_data_file: config.partition_columns_in_data_file.clone(),
                })
            }

            Some(EngineConfig::Buffer(config)) => Ok(ClickhouseEngine::Buffer(BufferEngine {
                target_database: config.target_database.clone(),
                target_table: config.target_table.clone(),
                num_layers: config.num_layers,
                min_time: config.min_time,
                max_time: config.max_time,
                min_rows: config.min_rows,
                max_rows: config.max_rows,
                min_bytes: config.min_bytes,
                max_bytes: config.max_bytes,
                flush_time: config.flush_time,
                flush_rows: config.flush_rows,
                flush_bytes: config.flush_bytes,
            })),

            Some(EngineConfig::Distributed(config)) => Ok(ClickhouseEngine::Distributed {
                cluster: config.cluster.clone(),
                target_database: config.target_database.clone(),
                target_table: config.target_table.clone(),
                sharding_key: config.sharding_key.clone(),
                policy_name: config.policy_name.clone(),
            }),

            Some(EngineConfig::IcebergS3(config)) => {
                // Keep environment variable markers as-is - credentials will be resolved at runtime
                Ok(ClickhouseEngine::IcebergS3 {
                    path: config.path.clone(),
                    format: config.format.clone(),
                    aws_access_key_id: config.aws_access_key_id.clone(),
                    aws_secret_access_key: config.aws_secret_access_key.clone(),
                    compression: config.compression.clone(),
                })
            }

            Some(EngineConfig::Kafka(config)) => Ok(ClickhouseEngine::Kafka {
                broker_list: config.broker_list.clone(),
                topic_list: config.topic_list.clone(),
                group_name: config.group_name.clone(),
                format: config.format.clone(),
            }),

            Some(EngineConfig::Merge {
                source_database,
                tables_regexp,
            }) => {
                // Resolve currentDatabase() to the actual database name so the desired state
                // matches what ClickHouse stores (it resolves this function at creation time).
                let resolved_db = if source_database == "currentDatabase()" {
                    default_database.to_string()
                } else {
                    source_database.clone()
                };
                Ok(ClickhouseEngine::Merge {
                    source_database: resolved_db,
                    tables_regexp: tables_regexp.clone(),
                })
            }

            None => Ok(ClickhouseEngine::MergeTree),
        }
    }
}

fn normalize_all_metadata_paths(infra_map: &mut InfrastructureMap, project_root: &Path) {
    for table in infra_map.tables.values_mut() {
        if let Some(metadata) = &mut table.metadata {
            metadata.normalize_source_path(project_root);
        }
    }

    for mv in infra_map.materialized_views.values_mut() {
        if let Some(metadata) = &mut mv.metadata {
            metadata.normalize_source_path(project_root);
        }
    }

    for view in infra_map.views.values_mut() {
        if let Some(metadata) = &mut view.metadata {
            metadata.normalize_source_path(project_root);
        }
    }

    // SqlResource has source_file directly, not in metadata struct
    for resource in infra_map.sql_resources.values_mut() {
        if let Some(source_file) = &mut resource.source_file {
            *source_file = normalize_path_string(source_file, project_root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base_table_json() -> serde_json::Value {
        json!({
            "name": "t1",
            "columns": [],
            "orderBy": ["id"]
        })
    }

    fn get_seed_filter(payload: serde_json::Value) -> SeedFilter {
        let partial: PartialInfrastructureMap =
            serde_json::from_value(payload).expect("payload should deserialize");
        partial
            .tables
            .get("t1")
            .expect("table t1 should exist")
            .seed_filter
            .clone()
    }

    #[test]
    fn seed_filter_missing_key_defaults() {
        let payload = json!({ "tables": { "t1": base_table_json() } });
        assert_eq!(get_seed_filter(payload), SeedFilter::default());
    }

    #[test]
    fn seed_filter_null_defaults() {
        let mut t = base_table_json();
        t.as_object_mut()
            .unwrap()
            .insert("seedFilter".into(), serde_json::Value::Null);
        let payload = json!({ "tables": { "t1": t } });
        assert_eq!(get_seed_filter(payload), SeedFilter::default());
    }

    #[test]
    fn seed_filter_camel_case() {
        let mut t = base_table_json();
        t.as_object_mut().unwrap().insert(
            "seedFilter".into(),
            json!({ "limit": 10, "where": "id > 0" }),
        );
        let payload = json!({ "tables": { "t1": t } });
        let sf = get_seed_filter(payload);
        assert_eq!(sf.limit, Some(10));
        assert_eq!(sf.where_clause.as_deref(), Some("id > 0"));
    }

    #[test]
    fn seed_filter_snake_case() {
        let mut t = base_table_json();
        t.as_object_mut()
            .unwrap()
            .insert("seed_filter".into(), json!({ "limit": 20 }));
        let payload = json!({ "tables": { "t1": t } });
        let sf = get_seed_filter(payload);
        assert_eq!(sf.limit, Some(20));
        assert_eq!(sf.where_clause, None);
    }
}
