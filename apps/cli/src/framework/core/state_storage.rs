//! State storage abstraction for InfrastructureMap
//!
//! This module provides an abstraction over where this tool stores its infrastructure state.
//! State is stored in ClickHouse.

use crate::framework::core::infrastructure_map::InfrastructureMap;
use crate::infrastructure::olap::clickhouse::config::ClickHouseConfig;
use crate::infrastructure::olap::clickhouse::ConfiguredDBClient;
use crate::infrastructure::olap::clickhouse::{check_ready, create_client};
use crate::project::Project;
use crate::utilities::machine_id::get_or_create_machine_id;
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use protobuf::Message;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Lock data for migration coordination
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationLock {
    pub machine_id: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Version of the infrastructure-map data model this CLI reads and writes.
///
/// Bump this when the set of resource kinds the map can represent changes.
/// It is deliberately NOT the CLI's release version: keying the check on that
/// meant a CLI released below the threshold refused the state it had just
/// written itself.
pub const CURRENT_DATA_MODEL_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StateCompatibilityError {
    #[error(
        "This project's stored infrastructure state predates this tool and may describe \
         streaming or workflow resources it cannot represent.\n\n\
         Continuing would plan the deletion of every topic and workflow recorded in that \
         state.\n\n\
         To proceed, delete the stored state rows:\n\n  \
         DELETE FROM `_MOOSE_STATE` WHERE key LIKE 'infra_map_%'\n\n\
         Your ClickHouse tables are not affected: with no stored map this tool reads the \
         live database and re-adopts the tables that already exist, so the next plan shows \
         no table drops.\n\n\
         What you lose is tracking of any streaming or workflow resources the previous tool \
         managed. They keep running, unmanaged, until you remove them yourself."
    )]
    NoDataModelVersion,

    #[error(
        "This project's stored infrastructure state uses data model version {found}, but \
         this tool understands version {understood}.\n\n\
         It was written by a newer release. Upgrade this tool rather than continuing — \
         proceeding would silently drop whatever the newer model represents."
    )]
    NewerDataModel { found: u32, understood: u32 },
}

/// Rejects stored state this CLI cannot safely diff against.
///
/// Deliberately independent of `CLI_VERSION`: the question is what shape the
/// stored data is in, not what the running binary is called or numbered.
fn check_stored_data_model(stored: Option<u32>) -> Result<(), StateCompatibilityError> {
    match stored {
        None => Err(StateCompatibilityError::NoDataModelVersion),
        Some(v) if v > CURRENT_DATA_MODEL_VERSION => Err(StateCompatibilityError::NewerDataModel {
            found: v,
            understood: CURRENT_DATA_MODEL_VERSION,
        }),
        Some(_) => Ok(()),
    }
}

/// Entry point used by the storage backend's load path.
pub fn check_state_compatibility(map: &InfrastructureMap) -> Result<(), StateCompatibilityError> {
    check_stored_data_model(map.data_model_version)
}

#[async_trait]
pub trait StateStorage: Send + Sync {
    /// Store the infrastructure map
    async fn store_infrastructure_map(&self, infra_map: &InfrastructureMap) -> Result<()>;

    /// Load the infrastructure map
    async fn load_infrastructure_map(&self) -> Result<Option<InfrastructureMap>>;

    /// Try to acquire migration lock
    /// Must be manually released with release_migration_lock()
    /// Lock automatically expires after 5 minutes as a safety fallback
    async fn acquire_migration_lock(&self) -> Result<()>;

    /// Release migration lock
    async fn release_migration_lock(&self) -> Result<()>;
}

/// ClickHouse-based state storage (for serverless/CLI-only deployments)
pub struct ClickHouseStateStorage {
    client: ConfiguredDBClient,
    db_name: String,
}

impl ClickHouseStateStorage {
    /// Deliberately still named `_MOOSE_STATE`.
    ///
    /// This table lives in the user's ClickHouse, not in this repository.
    /// Renaming it would orphan the stored infrastructure map of every
    /// project created before the rename — the tool would find no state,
    /// treat the database as empty, and plan to create tables that already
    /// exist. The cost of an inaccurate name is cosmetic; the cost of
    /// renaming it is other people's data.
    const STATE_TABLE: &'static str = "_MOOSE_STATE";
    const LOCK_KEY: &'static str = "migration_lock";
    const LOCK_TIMEOUT_SECS: i64 = 300; // 5 minutes

    pub fn new(client: ConfiguredDBClient, db_name: String) -> Self {
        Self { client, db_name }
    }

    /// Ensure the state table exists using KeeperMap for strong consistency
    async fn ensure_state_table(&self) -> Result<()> {
        // Use KeeperMap for:
        // 1. Atomic lock operations (prevents concurrent migrations)
        // 2. Synchronous writes (no async_insert race conditions)
        // 3. Immediate read-after-write consistency
        // 4. Already configured in dev mode. Available in Clickhouse Cloud
        let create_table_sql = format!(
            r#"
            CREATE TABLE IF NOT EXISTS `{}`.`{}`
            (
                key String,
                value String,
                created_at DateTime DEFAULT now()
            )
            ENGINE = KeeperMap('/{}/{}')
            PRIMARY KEY key
            "#,
            self.db_name,
            Self::STATE_TABLE,
            self.db_name,
            Self::STATE_TABLE
        );

        debug!("Creating KeeperMap state table: {}", create_table_sql);

        self.client
            .client
            .query(&create_table_sql)
            .execute()
            .await
            .context("Failed to create state table")?;

        Ok(())
    }
}

#[async_trait]
impl StateStorage for ClickHouseStateStorage {
    async fn store_infrastructure_map(&self, infra_map: &InfrastructureMap) -> Result<()> {
        // Ensure table exists
        self.ensure_state_table().await?;

        // Add version tracking before serialization
        let mut versioned_map = infra_map.clone();
        versioned_map.moose_version = Some(crate::utilities::constants::CLI_VERSION.to_string());
        versioned_map.data_model_version = Some(CURRENT_DATA_MODEL_VERSION);

        // Serialize to protobuf
        let encoded: Vec<u8> = versioned_map.to_proto().write_to_bytes()?;
        let encoded_base64 =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &encoded);

        // Use timestamp-based key for history
        let timestamp_ms = Utc::now().timestamp_millis();
        let key = format!("infra_map_{}", timestamp_ms);

        // Insert with timestamp key (creates audit history)
        let insert_sql = format!(
            "INSERT INTO `{}`.`{}` (key, value) VALUES ('{}', '{}')",
            self.db_name,
            Self::STATE_TABLE,
            key,
            encoded_base64
        );

        debug!(
            "Storing infrastructure map in ClickHouse KeeperMap state table (key: {})",
            key
        );

        self.client
            .client
            .query(&insert_sql)
            .execute()
            .await
            .context("Failed to store infrastructure map in ClickHouse")?;

        info!("Stored infrastructure map in ClickHouse ({})", key);

        Ok(())
    }

    async fn load_infrastructure_map(&self) -> Result<Option<InfrastructureMap>> {
        // Ensure table exists first
        self.ensure_state_table().await?;

        // Query for the latest state by timestamp
        let query_sql = format!(
            r#"
            SELECT value
            FROM `{}`.`{}`
            WHERE key LIKE 'infra_map_%'
            ORDER BY created_at DESC
            LIMIT 1
            "#,
            self.db_name,
            Self::STATE_TABLE
        );

        info!("Loading infrastructure map from database: {}", self.db_name);

        let mut cursor = self
            .client
            .client
            .query(&query_sql)
            .fetch::<String>()
            .context("Failed to query state table")?;

        // Try to get the first row
        let value_str = match cursor.next().await {
            Ok(Some(value)) => value,
            Ok(None) => {
                debug!("No infrastructure map found in ClickHouse state table");
                return Ok(None);
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to fetch row: {}", e));
            }
        };

        // Decode from base64
        let encoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &value_str)
                .context("Failed to decode base64 state value")?;

        // Deserialize from protobuf and canonicalize tables to handle backward compatibility
        // with data saved by older CLI versions (e.g., missing order_by)
        let infra_map = InfrastructureMap::from_proto(encoded)
            .context("Failed to deserialize infrastructure map from protobuf")?
            .canonicalize_tables();

        check_state_compatibility(&infra_map)?;

        info!("Loaded infrastructure map from ClickHouse");

        Ok(Some(infra_map))
    }

    async fn acquire_migration_lock(&self) -> Result<()> {
        self.ensure_state_table().await?;

        // Enable strict mode for this session - INSERT will fail if key exists (not overwrite)
        self.client
            .client
            .query("SET keeper_map_strict_mode = 1")
            .execute()
            .await
            .context("Failed to enable strict mode")?;

        // Check if lock exists
        let existing_lock_query = format!(
            "SELECT value FROM `{}`.`{}` WHERE key = '{}'",
            self.db_name,
            Self::STATE_TABLE,
            Self::LOCK_KEY
        );

        let mut cursor = self
            .client
            .client
            .query(&existing_lock_query)
            .fetch::<String>()
            .context("Failed to query for existing lock")?;

        if let Ok(Some(lock_json_base64)) = cursor.next().await {
            // Lock exists - check if expired
            // Base64 decode the lock data
            let lock_json_bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                lock_json_base64.as_bytes(),
            )
            .context("Failed to base64 decode lock data")?;
            let lock_json = String::from_utf8(lock_json_bytes)
                .context("Failed to convert lock data to UTF-8")?;

            let existing_lock: MigrationLock =
                serde_json::from_str(&lock_json).context("Failed to deserialize existing lock")?;

            if existing_lock.expires_at < Utc::now() {
                // Stale lock - delete it
                let delete_sql = format!(
                    "DELETE FROM `{}`.`{}` WHERE key = '{}'",
                    self.db_name,
                    Self::STATE_TABLE,
                    Self::LOCK_KEY
                );

                self.client
                    .client
                    .query(&delete_sql)
                    .execute()
                    .await
                    .context("Failed to delete stale lock")?;

                warn!(
                    "Deleted stale migration lock from machine {} (expired at {})",
                    existing_lock.machine_id, existing_lock.expires_at
                );
            } else {
                // Active lock held by someone else
                let time_remaining = existing_lock.expires_at - Utc::now();
                let minutes = time_remaining.num_minutes();
                let seconds = time_remaining.num_seconds() % 60;

                anyhow::bail!(
                    "Migration already in progress on machine {}. Started at {}. Expires in {}m {}s.",
                    existing_lock.machine_id,
                    existing_lock.started_at.format("%Y-%m-%d %H:%M:%S UTC"),
                    minutes,
                    seconds
                );
            }
        }

        // Try to acquire lock
        let lock_data = MigrationLock {
            machine_id: get_or_create_machine_id(),
            started_at: Utc::now(),
            expires_at: Utc::now() + Duration::seconds(Self::LOCK_TIMEOUT_SECS),
        };

        let lock_json =
            serde_json::to_string(&lock_data).context("Failed to serialize lock data")?;

        // Base64 encode to avoid SQL injection (no escaping needed for base64)
        let lock_json_base64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            lock_json.as_bytes(),
        );

        let insert_sql = format!(
            "INSERT INTO `{}`.`{}` (key, value) VALUES ('{}', '{}')",
            self.db_name,
            Self::STATE_TABLE,
            Self::LOCK_KEY,
            lock_json_base64
        );

        match self.client.client.query(&insert_sql).execute().await {
            Ok(_) => {
                info!(
                    "Acquired migration lock (expires in {} seconds)",
                    Self::LOCK_TIMEOUT_SECS
                );
                Ok(())
            }
            Err(e) => {
                // Race condition - someone else got the lock between our check and insert
                anyhow::bail!("Failed to acquire migration lock (race condition): {}", e)
            }
        }
    }

    async fn release_migration_lock(&self) -> Result<()> {
        let delete_sql = format!(
            "DELETE FROM `{}`.`{}` WHERE key = '{}'",
            self.db_name,
            Self::STATE_TABLE,
            Self::LOCK_KEY
        );

        self.client
            .client
            .query(&delete_sql)
            .execute()
            .await
            .context("Failed to release migration lock")?;

        info!("Released migration lock");
        Ok(())
    }
}

/// Builder for creating state storage based on project configuration.
///
/// Storage backend is determined by `state_config.storage` in tch.config.toml.
pub struct StateStorageBuilder<'a> {
    project: &'a Project,
    clickhouse_config: Option<ClickHouseConfig>,
}

impl<'a> StateStorageBuilder<'a> {
    pub fn from_config(project: &'a Project) -> Self {
        Self {
            project,
            clickhouse_config: None,
        }
    }

    /// Provide a ClickHouse config (for serverless migrations with remote ClickHouse)
    pub fn clickhouse_config(mut self, clickhouse_config: Option<ClickHouseConfig>) -> Self {
        self.clickhouse_config = clickhouse_config;
        self
    }

    pub async fn build(self) -> Result<Box<dyn StateStorage>> {
        match self.project.state_config.storage.as_str() {
            "clickhouse" => {
                let clickhouse_config = self.clickhouse_config.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Internal error: ClickHouse state storage builder called without config. \
                         This should have been provided by the caller via .clickhouse_config(Some(...))."
                    )
                })?;

                let client = create_client(clickhouse_config.clone());
                check_ready(&client).await?;
                Ok(Box::new(ClickHouseStateStorage::new(
                    client,
                    clickhouse_config.db_name.clone(),
                )))
            }
            _ => anyhow::bail!(
                "Unknown state storage backend '{}' in project configuration. \
                 The only supported option is \"clickhouse\"",
                self.project.state_config.storage
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_map_with_no_data_model_version() {
        // A map written by any pre-0.1.0 CLI: it has no data_model_version at
        // all, because that field did not exist. It may describe topics and
        // workflows this CLI cannot represent.
        let err = check_stored_data_model(None).unwrap_err();
        assert!(err.to_string().contains("_MOOSE_STATE"));
    }

    #[test]
    fn rejects_a_map_from_a_newer_data_model() {
        // Forward incompatibility: a future CLI wrote a map this one cannot
        // fully represent. Refusing is safer than silently dropping fields.
        let err = check_stored_data_model(Some(CURRENT_DATA_MODEL_VERSION + 1)).unwrap_err();
        assert!(err.to_string().contains("newer"));
    }

    #[test]
    fn accepts_a_map_from_the_current_data_model() {
        assert!(check_stored_data_model(Some(CURRENT_DATA_MODEL_VERSION)).is_ok());
    }

    #[test]
    fn the_guard_is_independent_of_the_cli_version() {
        // The whole point of this redesign: the check must not consult
        // CLI_VERSION, so shipping 0.1.0 is safe.
        assert!(check_stored_data_model(Some(CURRENT_DATA_MODEL_VERSION)).is_ok());
        assert!(check_stored_data_model(None).is_err());
    }

    #[test]
    fn accepts_a_map_written_by_this_cli() {
        let map = InfrastructureMap {
            moose_version: Some(crate::utilities::constants::CLI_VERSION.to_string()),
            data_model_version: Some(CURRENT_DATA_MODEL_VERSION),
            ..Default::default()
        };
        assert!(check_state_compatibility(&map).is_ok());
    }
}
