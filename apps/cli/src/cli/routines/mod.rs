//! # Routines [Deprecation warning]
//!
//! *****
//! Routines that get run by a CLI should simply be a function that returns a routine success or routine failure. Do not use
//! the Routine and Routine controller structs and traits
//! *****
//!
//!
//! This module is used to define routines that can be run by the CLI. Routines are a collection of operations that are run in
//! sequence. They can be run silently or explicitly. When run explicitly, they display messages to the user. When run silently,
//! they do not display any messages to the user.
//!
//! ## RoutineSuccess
//! The `RoutineSuccess` struct is used to return a successful result from a routine. It contains a `Message` and a `MessageType`.
//!
//! ## RoutineFailure
//! The `RoutineFailure` struct is used to return a failure result from a routine. It contains a `Message`, a `MessageType`, and an
//! `Error`.

use crate::framework::core::infrastructure_map::InfrastructureMap;
use crate::framework::core::migration_plan::{MigrationPlan, MigrationPlanWithBeforeAfter};
use crate::framework::core::plan::InfraPlan;
use crate::framework::core::plan::ReconciliationFilter;
use crate::framework::core::plan_validator;
use crate::framework::core::state_storage::StateStorageBuilder;
use crate::infrastructure::olap::clickhouse::diff_strategy::ClickHouseTableDiffStrategy;
use crate::project::Project;

use super::{display, Message, MessageType};

pub mod auth;
pub mod build;
pub mod code_generation;
pub mod format_query;
pub mod logs;
pub mod ls;
pub mod migrate;
pub mod peek;
pub mod query;
pub mod seed_data;
pub mod truncate_table;

#[derive(Debug, Clone)]
#[must_use = "The message should be displayed."]
pub struct RoutineSuccess {
    pub message: Message,
    pub message_type: MessageType,
}
impl From<RoutineFailure> for anyhow::Error {
    fn from(failure: RoutineFailure) -> Self {
        if let Some(err) = failure.error {
            err
        } else {
            anyhow::anyhow!("{}: {}", failure.message.action, failure.message.details)
        }
    }
}

// Implement success and info contructors and a new constructor that lets the user choose which type of message to display
impl RoutineSuccess {
    pub fn success(message: Message) -> Self {
        Self {
            message,
            message_type: MessageType::Success,
        }
    }
}

#[derive(Debug)]
pub struct RoutineFailure {
    pub message: Message,
    pub message_type: MessageType,
    pub error: Option<anyhow::Error>,
}
impl RoutineFailure {
    pub fn new<F: Into<anyhow::Error>>(message: Message, error: F) -> Self {
        Self {
            message,
            message_type: MessageType::Error,
            error: Some(error.into()),
        }
    }

    /// create a RoutineFailure error without an error
    pub fn error(message: Message) -> Self {
        Self {
            message,
            message_type: MessageType::Error,
            error: None,
        }
    }
}

/// Compares the local project code with the deployed ClickHouse infrastructure
/// and displays the resulting plan.
///
/// # Arguments
/// * `project` - Reference to the project
/// * `clickhouse_url` - Connection string for the deployed ClickHouse
/// * `json` - Emit the plan as JSON on stdout instead of a formatted diff
pub async fn remote_plan(
    project: &Project,
    clickhouse_url: &str,
    json: bool,
) -> anyhow::Result<()> {
    let local_infra_map = crate::framework::core::plan::load_target_infrastructure(project).await?;

    if !json {
        display::show_message_wrapper(
            MessageType::Info,
            Message {
                action: "Remote Plan".to_string(),
                details: "Comparing local project code with deployed infrastructure".to_string(),
            },
        );
    }

    let filter = ReconciliationFilter::from_infra_map(&local_infra_map);
    let remote_infra_map = get_remote_inframap_serverless(project, clickhouse_url, &filter).await?;

    tracing::info!("Remote inframap: {} tables", remote_infra_map.tables.len());
    tracing::info!("Local inframap: {} tables", local_infra_map.tables.len());

    // Normalize SQL in both maps before diffing to handle ClickHouse reformatting
    let olap_client =
        crate::infrastructure::olap::clickhouse::create_client(project.clickhouse_config.clone());
    let remote_normalized = crate::framework::core::plan::normalize_infra_map_for_comparison(
        &remote_infra_map,
        &olap_client,
    )
    .await;
    let local_normalized = crate::framework::core::plan::normalize_infra_map_for_comparison(
        &local_infra_map,
        &olap_client,
    )
    .await;

    let clickhouse_strategy = ClickHouseTableDiffStrategy;

    // Remote plan always uses production settings: respect_lifecycle=true, is_production=true
    let changes = remote_normalized.diff_with_table_strategy(
        &local_normalized,
        &clickhouse_strategy,
        true, // respect_lifecycle
        true, // is_production
        &project.migration_config.ignore_operations,
    );

    if !json {
        display::show_message_wrapper(
            MessageType::Success,
            Message {
                action: "Remote Plan".to_string(),
                details: "Calculated plan differences locally".to_string(),
            },
        );
    }

    if changes.is_empty() {
        if json {
            // Output empty plan as JSON
            let temp_plan = InfraPlan {
                changes,
                target_infra_map: local_infra_map,
            };
            println!("{}", serde_json::to_string_pretty(&temp_plan)?);
        } else {
            display::show_message_wrapper(
                MessageType::Info,
                Message {
                    action: "No Changes".to_string(),
                    details: "No changes detected".to_string(),
                },
            );
        }
        return Ok(());
    }

    // Create a temporary InfraPlan to use with the show_changes function
    let temp_plan = InfraPlan {
        changes,
        target_infra_map: local_infra_map,
    };

    if json {
        // ONLY output JSON to stdout - no other messages
        println!("{}", serde_json::to_string_pretty(&temp_plan)?);
    } else {
        display::show_changes(&temp_plan);
    }
    Ok(())
}

pub async fn remote_gen_migration(
    project: &Project,
    clickhouse_url: &str,
) -> anyhow::Result<MigrationPlanWithBeforeAfter> {
    let local_infra_map = crate::framework::core::plan::load_target_infrastructure(project).await?;

    display::show_message_wrapper(
        MessageType::Info,
        Message {
            action: "Remote Plan".to_string(),
            details: "Comparing local project code with deployed infrastructure".to_string(),
        },
    );

    let filter = ReconciliationFilter::from_infra_map(&local_infra_map);
    let remote_infra_map = get_remote_inframap_serverless(project, clickhouse_url, &filter).await?;

    // Normalize SQL in both maps before diffing to handle ClickHouse reformatting
    let olap_client =
        crate::infrastructure::olap::clickhouse::create_client(project.clickhouse_config.clone());
    let remote_normalized = crate::framework::core::plan::normalize_infra_map_for_comparison(
        &remote_infra_map,
        &olap_client,
    )
    .await;
    let local_normalized = crate::framework::core::plan::normalize_infra_map_for_comparison(
        &local_infra_map,
        &olap_client,
    )
    .await;

    // Calculate changes using the same strategy as remote_plan
    let clickhouse_strategy = ClickHouseTableDiffStrategy;

    // Migration generation uses production settings: respect_lifecycle=true, is_production=true
    let changes = remote_normalized.diff_with_table_strategy(
        &local_normalized,
        &clickhouse_strategy,
        true, // respect_lifecycle
        true, // is_production
        &project.migration_config.ignore_operations,
    );

    display::show_message_wrapper(
        MessageType::Success,
        Message {
            action: "Remote Plan".to_string(),
            details: "Calculated plan differences locally".to_string(),
        },
    );

    // Validate the plan before generating migration files
    let plan = InfraPlan {
        target_infra_map: local_infra_map.clone(),
        changes,
    };

    plan_validator::validate(project, &plan)?;

    let db_migration =
        MigrationPlan::from_infra_plan(&plan.changes, &project.clickhouse_config.db_name)?;

    Ok(MigrationPlanWithBeforeAfter {
        remote_state: remote_infra_map,
        local_infra_map,
        db_migration,
    })
}

/// Get remote infrastructure map for serverless deployments
///
/// Loads state from ClickHouse, then reconciles with the actual ClickHouse schema
async fn get_remote_inframap_serverless(
    project: &Project,
    clickhouse_url: &str,
    filter: &ReconciliationFilter,
) -> anyhow::Result<InfrastructureMap> {
    use crate::infrastructure::olap::clickhouse::config::parse_clickhouse_connection_string;
    use crate::infrastructure::olap::clickhouse::create_client;

    let clickhouse_config = parse_clickhouse_connection_string(clickhouse_url)?;

    // Build state storage based on config
    let state_storage = StateStorageBuilder::from_config(project)
        .clickhouse_config(Some(clickhouse_config.clone()))
        .build()
        .await?;

    let olap_client = create_client(clickhouse_config.clone());

    let reconciled_infra_map = crate::framework::core::plan::load_reconciled_infrastructure(
        project,
        &*state_storage,
        olap_client,
        filter,
    )
    .await?;

    Ok(reconciled_infra_map)
}
