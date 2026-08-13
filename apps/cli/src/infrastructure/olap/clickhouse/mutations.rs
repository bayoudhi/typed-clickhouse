//! Serialization of mutation-producing ALTERs during a migration.
//!
//! ClickHouse executes `ALTER TABLE ... MODIFY COLUMN`, `DROP PROJECTION` and
//! friends asynchronously: the statement returns as soon as the mutation is
//! *queued*. A migration plan that touches the same table several times
//! therefore stacks unfinished mutations, and ClickHouse rejects the next
//! ALTER with:
//!
//! ```text
//! Code: 517. DB::Exception: Previous 2 mutation queries are not finished yet.
//! ... Probably too many alters executing concurrently (highly not recommended).
//! You can retry this error. (CANNOT_ASSIGN_ALTER)
//! ```
//!
//! This module provides the barrier that prevents it: before issuing a
//! mutation-producing operation, wait until the target table has no unfinished
//! mutations, and retry the operation if ClickHouse still rejects it.

use std::error::Error;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use super::{ClickhouseChangesError, ConfiguredDBClient, SerializableOlapOperation};

/// Substring identifying the `CANNOT_ASSIGN_ALTER` error code in a ClickHouse
/// server response.
const ALTER_CONFLICT_ERROR_CODE: &str = "Code: 517";

/// Symbolic name ClickHouse appends to a `CANNOT_ASSIGN_ALTER` response.
const ALTER_CONFLICT_ERROR_NAME: &str = "CANNOT_ASSIGN_ALTER";

/// How long to wait for a table's in-flight mutations to drain before giving up.
///
/// `MODIFY COLUMN` on a large table rewrites every part, so this is generous.
pub const DEFAULT_MUTATION_WAIT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How often to re-check `system.mutations` while waiting.
pub const DEFAULT_MUTATION_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How many times to re-issue an operation ClickHouse rejected with
/// `CANNOT_ASSIGN_ALTER`. Each retry re-runs the wait barrier first.
pub const DEFAULT_ALTER_CONFLICT_RETRIES: u32 = 5;

/// Tuning for the mutation barrier applied around each migration operation.
#[derive(Debug, Clone, Copy)]
pub struct MutationWaitConfig {
    /// Maximum time to wait for a table's mutations to drain.
    pub timeout: Duration,
    /// Interval between `system.mutations` polls.
    pub poll_interval: Duration,
    /// Number of `CANNOT_ASSIGN_ALTER` retries before failing.
    pub max_retries: u32,
}

impl Default for MutationWaitConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_MUTATION_WAIT_TIMEOUT,
            poll_interval: DEFAULT_MUTATION_POLL_INTERVAL,
            max_retries: DEFAULT_ALTER_CONFLICT_RETRIES,
        }
    }
}

/// An unfinished mutation reported by `system.mutations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMutation {
    /// ClickHouse's identifier for the mutation, e.g. `0000002214`.
    pub mutation_id: String,
    /// The mutation command, e.g. `(DROP PROJECTION IF EXISTS p_by_unit)`.
    pub command: String,
    /// Why the mutation last failed, empty when it has not failed.
    pub latest_fail_reason: String,
}

/// Error returned when `system.mutations` cannot be read.
#[derive(Debug, thiserror::Error)]
#[error("failed to read pending mutations for `{database}`.`{table}`")]
pub struct MutationQueryError {
    /// Database of the table being polled.
    pub database: String,
    /// Table being polled.
    pub table: String,
    /// The underlying failure.
    #[source]
    pub source: Box<dyn Error + Send + Sync>,
}

/// Error returned by [`wait_for_mutations`].
#[derive(Debug, thiserror::Error)]
pub enum MutationWaitError {
    /// `system.mutations` could not be read.
    #[error(transparent)]
    Query(#[from] MutationQueryError),

    /// The table still had unfinished mutations when the timeout elapsed.
    #[error(
        "timed out after {}s waiting for in-flight mutation(s) on `{database}`.`{table}` to finish:\n{}",
        .waited.as_secs(),
        format_pending(.pending)
    )]
    Timeout {
        /// Database of the table that never drained.
        database: String,
        /// Table that never drained.
        table: String,
        /// How long we waited.
        waited: Duration,
        /// The mutations still outstanding when we gave up.
        pending: Vec<PendingMutation>,
    },
}

/// Error returned by [`apply_operation_with_mutation_barrier`].
#[derive(Debug, thiserror::Error)]
pub enum ApplyOperationError {
    /// Waiting for the table's mutations to drain failed.
    #[error(transparent)]
    Wait(#[from] MutationWaitError),

    /// The operation itself failed.
    #[error(transparent)]
    Execute(#[from] ClickhouseChangesError),
}

impl From<ApplyOperationError> for ClickhouseChangesError {
    fn from(error: ApplyOperationError) -> Self {
        match error {
            ApplyOperationError::Wait(wait) => Self::MutationWait(wait),
            ApplyOperationError::Execute(execute) => execute,
        }
    }
}

/// Renders pending mutations for inclusion in an error message.
fn format_pending(pending: &[PendingMutation]) -> String {
    pending
        .iter()
        .map(|m| {
            if m.latest_fail_reason.is_empty() {
                format!("  • {} {}", m.mutation_id, m.command)
            } else {
                format!(
                    "  • {} {} (last failure: {})",
                    m.mutation_id, m.command, m.latest_fail_reason
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads the unfinished mutations of a single table.
#[async_trait]
pub trait MutationQuery: Send + Sync {
    /// Returns the mutations of `database`.`table` that have not finished yet.
    async fn pending_mutations(
        &self,
        database: &str,
        table: &str,
    ) -> Result<Vec<PendingMutation>, MutationQueryError>;
}

/// Executes a single migration operation.
#[async_trait]
pub trait OperationExecutor: Send + Sync {
    /// Applies `operation` to the database.
    async fn execute(
        &self,
        operation: &SerializableOlapOperation,
    ) -> Result<(), ClickhouseChangesError>;
}

/// Row shape returned by [`PENDING_MUTATIONS_QUERY`].
#[derive(clickhouse::Row, serde::Deserialize)]
struct PendingMutationRow {
    mutation_id: String,
    command: String,
    latest_fail_reason: String,
}

#[async_trait]
impl MutationQuery for ConfiguredDBClient {
    async fn pending_mutations(
        &self,
        database: &str,
        table: &str,
    ) -> Result<Vec<PendingMutation>, MutationQueryError> {
        self.client
            .query(PENDING_MUTATIONS_QUERY)
            .bind(database)
            .bind(table)
            .fetch_all::<PendingMutationRow>()
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| PendingMutation {
                        mutation_id: row.mutation_id,
                        command: row.command,
                        latest_fail_reason: row.latest_fail_reason,
                    })
                    .collect()
            })
            .map_err(|error| MutationQueryError {
                database: database.to_string(),
                table: table.to_string(),
                source: Box::new(error),
            })
    }
}

/// Runs migration operations against a live ClickHouse connection.
pub struct AtomicOperationExecutor<'a> {
    client: &'a ConfiguredDBClient,
    db_name: &'a str,
    is_dev: bool,
}

impl<'a> AtomicOperationExecutor<'a> {
    /// Creates an executor bound to a connection.
    ///
    /// # Arguments
    /// * `client` - The connection to run operations on
    /// * `db_name` - Database used by operations that do not name one
    /// * `is_dev` - Whether the project is running in development mode
    pub fn new(client: &'a ConfiguredDBClient, db_name: &'a str, is_dev: bool) -> Self {
        Self {
            client,
            db_name,
            is_dev,
        }
    }
}

#[async_trait]
impl OperationExecutor for AtomicOperationExecutor<'_> {
    async fn execute(
        &self,
        operation: &SerializableOlapOperation,
    ) -> Result<(), ClickhouseChangesError> {
        super::execute_atomic_operation(self.db_name, operation, self.client, self.is_dev).await
    }
}

/// Returns the `(database, table)` an operation issues an `ALTER TABLE`
/// against, or `None` for operations that cannot queue a mutation.
///
/// Every table-level ALTER is included, not only the ones that always produce a
/// mutation: ClickHouse counts unfinished metadata alters towards the same
/// limit, so metadata-only statements are rejected by the very conflict this
/// barrier exists to avoid.
///
/// # Arguments
/// * `operation` - The operation about to be executed
/// * `default_database` - Database used by operations that do not name one
pub fn mutation_target(
    operation: &SerializableOlapOperation,
    default_database: &str,
) -> Option<(String, String)> {
    let (table, database) = match operation {
        SerializableOlapOperation::AddTableColumn {
            table, database, ..
        }
        | SerializableOlapOperation::DropTableColumn {
            table, database, ..
        }
        | SerializableOlapOperation::ModifyTableColumn {
            table, database, ..
        }
        | SerializableOlapOperation::RenameTableColumn {
            table, database, ..
        }
        | SerializableOlapOperation::ModifyTableSettings {
            table, database, ..
        }
        | SerializableOlapOperation::ModifyTableTtl {
            table, database, ..
        }
        | SerializableOlapOperation::AddTableIndex {
            table, database, ..
        }
        | SerializableOlapOperation::DropTableIndex {
            table, database, ..
        }
        | SerializableOlapOperation::AddTableProjection {
            table, database, ..
        }
        | SerializableOlapOperation::DropTableProjection {
            table, database, ..
        }
        | SerializableOlapOperation::ModifySampleBy {
            table, database, ..
        }
        | SerializableOlapOperation::RemoveSampleBy {
            table, database, ..
        } => (table, database),
        _ => return None,
    };

    Some((
        database
            .clone()
            .unwrap_or_else(|| default_database.to_string()),
        table.clone(),
    ))
}

/// Lists a table's unfinished mutations. Binds the database and table names as
/// parameters, in that order.
pub const PENDING_MUTATIONS_QUERY: &str = "SELECT mutation_id, command, latest_fail_reason \
                                           FROM system.mutations \
                                           WHERE database = ? AND table = ? AND is_done = 0 \
                                           ORDER BY create_time";

/// Reports whether an error chain carries ClickHouse's `CANNOT_ASSIGN_ALTER`
/// response, which the server itself documents as retryable.
///
/// # Arguments
/// * `error` - The error to inspect, including its `source()` chain
pub fn is_alter_conflict(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(err) = current {
        let text = err.to_string();
        if text.contains(ALTER_CONFLICT_ERROR_CODE) || text.contains(ALTER_CONFLICT_ERROR_NAME) {
            return true;
        }
        current = err.source();
    }
    false
}

/// Blocks until `database`.`table` has no unfinished mutations.
///
/// Returns immediately when the table is already clear.
///
/// # Arguments
/// * `query` - Source of `system.mutations` rows
/// * `database` - Database of the table to wait on
/// * `table` - Table to wait on
/// * `config` - Timeout and poll interval
pub async fn wait_for_mutations<Q: MutationQuery + ?Sized>(
    query: &Q,
    database: &str,
    table: &str,
    config: &MutationWaitConfig,
) -> Result<(), MutationWaitError> {
    let started = Instant::now();
    let mut announced = false;

    loop {
        let pending = query.pending_mutations(database, table).await?;
        if pending.is_empty() {
            if announced {
                println!("      ✓ in-flight mutations on '{database}.{table}' finished");
            }
            return Ok(());
        }

        let waited = started.elapsed();
        if waited >= config.timeout {
            return Err(MutationWaitError::Timeout {
                database: database.to_string(),
                table: table.to_string(),
                waited,
                pending,
            });
        }

        if !announced {
            announced = true;
            println!(
                "      ⏳ waiting for {} in-flight mutation(s) on '{database}.{table}'...",
                pending.len()
            );
        }

        tokio::time::sleep(config.poll_interval).await;
    }
}

/// Applies one migration operation, serialising it against the target table's
/// in-flight mutations and retrying ClickHouse's retryable alter conflict.
///
/// # Arguments
/// * `query` - Source of `system.mutations` rows
/// * `executor` - Runs the operation
/// * `operation` - The operation to apply
/// * `default_database` - Database used by operations that do not name one
/// * `config` - Wait and retry tuning
pub async fn apply_operation_with_mutation_barrier<Q, E>(
    query: &Q,
    executor: &E,
    operation: &SerializableOlapOperation,
    default_database: &str,
    config: &MutationWaitConfig,
) -> Result<(), ApplyOperationError>
where
    Q: MutationQuery + ?Sized,
    E: OperationExecutor + ?Sized,
{
    let target = mutation_target(operation, default_database);

    let mut attempt = 0;
    loop {
        if let Some((database, table)) = &target {
            wait_for_mutations(query, database, table, config).await?;
        }

        match executor.execute(operation).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                if attempt >= config.max_retries || !is_alter_conflict(&error) {
                    return Err(ApplyOperationError::Execute(error));
                }
                attempt += 1;
                tracing::warn!(
                    "ClickHouse rejected the operation with a retryable alter conflict; retrying ({}/{}): {}",
                    attempt,
                    config.max_retries,
                    error
                );
                println!(
                    "      ↻ ClickHouse is still applying earlier alters; retrying ({}/{})",
                    attempt, config.max_retries
                );
                tokio::time::sleep(config.poll_interval).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::framework::core::infrastructure::table::{Column, ColumnType};

    fn column(name: &str) -> Column {
        Column {
            name: name.to_string(),
            data_type: ColumnType::String,
            required: true,
            unique: false,
            primary_key: false,
            default: None,
            annotations: vec![],
            comment: None,
            materialized: None,
            alias: None,
            ttl: None,
            codec: None,
        }
    }

    fn modify_column_op(table: &str, database: Option<&str>) -> SerializableOlapOperation {
        SerializableOlapOperation::ModifyTableColumn {
            table: table.to_string(),
            before_column: column("arrivalDate"),
            after_column: column("arrivalDate"),
            database: database.map(str::to_string),
            cluster_name: None,
        }
    }

    fn pending(id: &str, command: &str) -> PendingMutation {
        PendingMutation {
            mutation_id: id.to_string(),
            command: command.to_string(),
            latest_fail_reason: String::new(),
        }
    }

    /// Records every poll and replays a scripted sequence of responses.
    struct ScriptedMutations {
        responses: Mutex<Vec<Vec<PendingMutation>>>,
        polls: Mutex<Vec<(String, String)>>,
    }

    impl ScriptedMutations {
        fn new(responses: Vec<Vec<PendingMutation>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                polls: Mutex::new(Vec::new()),
            }
        }

        fn always_pending(mutation: PendingMutation) -> Self {
            Self {
                responses: Mutex::new(vec![]),
                polls: Mutex::new(Vec::new()),
            }
            .with_fallback(mutation)
        }

        fn with_fallback(self, mutation: PendingMutation) -> Self {
            let mut responses = self.responses.into_inner().unwrap();
            responses.push(vec![mutation]);
            Self {
                responses: Mutex::new(responses),
                polls: self.polls,
            }
        }

        fn poll_count(&self) -> usize {
            self.polls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl MutationQuery for ScriptedMutations {
        async fn pending_mutations(
            &self,
            database: &str,
            table: &str,
        ) -> Result<Vec<PendingMutation>, MutationQueryError> {
            self.polls
                .lock()
                .unwrap()
                .push((database.to_string(), table.to_string()));
            let mut responses = self.responses.lock().unwrap();
            if responses.len() > 1 {
                Ok(responses.remove(0))
            } else {
                Ok(responses.first().cloned().unwrap_or_default())
            }
        }
    }

    /// Always reports a clear table.
    struct NoMutations;

    #[async_trait]
    impl MutationQuery for NoMutations {
        async fn pending_mutations(
            &self,
            _database: &str,
            _table: &str,
        ) -> Result<Vec<PendingMutation>, MutationQueryError> {
            Ok(vec![])
        }
    }

    /// Fails with a `CANNOT_ASSIGN_ALTER` response a fixed number of times.
    struct ConflictingExecutor {
        failures_remaining: Mutex<u32>,
        attempts: Mutex<u32>,
    }

    impl ConflictingExecutor {
        fn new(failures: u32) -> Self {
            Self {
                failures_remaining: Mutex::new(failures),
                attempts: Mutex::new(0),
            }
        }

        fn attempts(&self) -> u32 {
            *self.attempts.lock().unwrap()
        }
    }

    #[async_trait]
    impl OperationExecutor for ConflictingExecutor {
        async fn execute(
            &self,
            _operation: &SerializableOlapOperation,
        ) -> Result<(), ClickhouseChangesError> {
            *self.attempts.lock().unwrap() += 1;
            let mut remaining = self.failures_remaining.lock().unwrap();
            if *remaining == 0 {
                return Ok(());
            }
            *remaining -= 1;
            Err(ClickhouseChangesError::NotSupported(
                "bad response: Code: 517. DB::Exception: Previous 2 mutation queries are not \
                 finished yet. Probably too many alters executing concurrently (highly not \
                 recommended). You can retry this error. (CANNOT_ASSIGN_ALTER)"
                    .to_string(),
            ))
        }
    }

    /// Fails with an error unrelated to alter conflicts.
    struct AlwaysFailingExecutor {
        attempts: Mutex<u32>,
    }

    impl AlwaysFailingExecutor {
        fn new() -> Self {
            Self {
                attempts: Mutex::new(0),
            }
        }

        fn attempts(&self) -> u32 {
            *self.attempts.lock().unwrap()
        }
    }

    #[async_trait]
    impl OperationExecutor for AlwaysFailingExecutor {
        async fn execute(
            &self,
            _operation: &SerializableOlapOperation,
        ) -> Result<(), ClickhouseChangesError> {
            *self.attempts.lock().unwrap() += 1;
            Err(ClickhouseChangesError::NotSupported(
                "Code: 47. DB::Exception: Unknown identifier".to_string(),
            ))
        }
    }

    fn fast_config() -> MutationWaitConfig {
        MutationWaitConfig {
            timeout: Duration::from_millis(200),
            poll_interval: Duration::from_millis(1),
            max_retries: 3,
        }
    }

    #[test]
    fn modify_column_targets_its_own_table() {
        let op = modify_column_op("reservations", Some("analytics"));
        assert_eq!(
            mutation_target(&op, "fallback"),
            Some(("analytics".to_string(), "reservations".to_string()))
        );
    }

    #[test]
    fn operation_without_database_falls_back_to_the_default() {
        let op = modify_column_op("reservations", None);
        assert_eq!(
            mutation_target(&op, "fallback"),
            Some(("fallback".to_string(), "reservations".to_string()))
        );
    }

    #[test]
    fn drop_projection_is_a_mutation_target() {
        let op = SerializableOlapOperation::DropTableProjection {
            table: "reservations".to_string(),
            projection_name: "p_by_unit".to_string(),
            database: None,
            cluster_name: None,
        };
        assert_eq!(
            mutation_target(&op, "local"),
            Some(("local".to_string(), "reservations".to_string()))
        );
    }

    #[test]
    fn view_operations_are_not_mutation_targets() {
        let op = SerializableOlapOperation::DropView {
            name: "v_reservations".to_string(),
            database: None,
        };
        assert_eq!(mutation_target(&op, "local"), None);
    }

    #[test]
    fn create_table_is_not_a_mutation_target() {
        let op = SerializableOlapOperation::DropTable {
            table: "reservations".to_string(),
            database: None,
            cluster_name: None,
        };
        assert_eq!(mutation_target(&op, "local"), None);
    }

    #[test]
    fn pending_mutations_query_selects_only_unfinished_rows() {
        assert_eq!(
            PENDING_MUTATIONS_QUERY,
            "SELECT mutation_id, command, latest_fail_reason \
             FROM system.mutations \
             WHERE database = ? AND table = ? AND is_done = 0 \
             ORDER BY create_time"
        );
    }

    #[test]
    fn alter_conflict_is_detected_in_the_error_chain() {
        let error = ClickhouseChangesError::NotSupported(
            "bad response: Code: 517. DB::Exception: Previous 2 mutation queries are not finished \
             yet. (CANNOT_ASSIGN_ALTER)"
                .to_string(),
        );
        assert!(is_alter_conflict(&error));
    }

    #[test]
    fn unrelated_errors_are_not_alter_conflicts() {
        let error = ClickhouseChangesError::NotSupported(
            "Code: 60. DB::Exception: Table does not exist".to_string(),
        );
        assert!(!is_alter_conflict(&error));
    }

    #[tokio::test]
    async fn waiting_returns_immediately_when_no_mutations_are_pending() {
        let query = ScriptedMutations::new(vec![vec![]]);
        wait_for_mutations(&query, "local", "reservations", &fast_config())
            .await
            .unwrap();
        assert_eq!(query.poll_count(), 1);
    }

    #[tokio::test]
    async fn waiting_polls_until_mutations_drain() {
        let query = ScriptedMutations::new(vec![
            vec![pending(
                "0000002214",
                "(DROP PROJECTION IF EXISTS p_by_unit)",
            )],
            vec![pending(
                "0000002214",
                "(DROP PROJECTION IF EXISTS p_by_unit)",
            )],
            vec![],
        ]);

        wait_for_mutations(&query, "local", "reservations", &fast_config())
            .await
            .unwrap();

        assert_eq!(query.poll_count(), 3);
    }

    #[tokio::test]
    async fn waiting_times_out_and_reports_the_outstanding_mutations() {
        let query = ScriptedMutations::always_pending(pending(
            "0000002215",
            "(MODIFY COLUMN IF EXISTS `arrivalDate` DateTime64(3))",
        ));

        let error = wait_for_mutations(&query, "local", "reservations", &fast_config())
            .await
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("0000002215"), "got: {message}");
        assert!(message.contains("`local`.`reservations`"), "got: {message}");
    }

    #[tokio::test]
    async fn applying_a_mutating_operation_waits_for_the_table_first() {
        let query = ScriptedMutations::new(vec![
            vec![pending(
                "0000002214",
                "(DROP PROJECTION IF EXISTS p_by_unit)",
            )],
            vec![],
        ]);
        let executor = ConflictingExecutor::new(0);

        apply_operation_with_mutation_barrier(
            &query,
            &executor,
            &modify_column_op("reservations", None),
            "local",
            &fast_config(),
        )
        .await
        .unwrap();

        assert_eq!(query.poll_count(), 2);
        assert_eq!(executor.attempts(), 1);
    }

    #[tokio::test]
    async fn applying_a_non_mutating_operation_skips_the_wait() {
        let query = ScriptedMutations::new(vec![vec![]]);
        let executor = ConflictingExecutor::new(0);

        apply_operation_with_mutation_barrier(
            &query,
            &executor,
            &SerializableOlapOperation::DropView {
                name: "v_reservations".to_string(),
                database: None,
            },
            "local",
            &fast_config(),
        )
        .await
        .unwrap();

        assert_eq!(query.poll_count(), 0);
        assert_eq!(executor.attempts(), 1);
    }

    #[tokio::test]
    async fn alter_conflicts_are_retried_until_they_succeed() {
        let executor = ConflictingExecutor::new(2);

        apply_operation_with_mutation_barrier(
            &NoMutations,
            &executor,
            &modify_column_op("reservations", None),
            "local",
            &fast_config(),
        )
        .await
        .unwrap();

        assert_eq!(executor.attempts(), 3);
    }

    #[tokio::test]
    async fn alter_conflicts_stop_after_the_retry_budget() {
        let executor = ConflictingExecutor::new(u32::MAX);

        let error = apply_operation_with_mutation_barrier(
            &NoMutations,
            &executor,
            &modify_column_op("reservations", None),
            "local",
            &fast_config(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ApplyOperationError::Execute(_)));
        assert_eq!(executor.attempts(), 4, "one initial attempt plus 3 retries");
    }

    #[test]
    fn wait_failures_convert_into_a_clickhouse_changes_error() {
        let wait = MutationWaitError::Timeout {
            database: "local".to_string(),
            table: "reservations".to_string(),
            waited: Duration::from_secs(1),
            pending: vec![pending("0000002215", "(MODIFY COLUMN `arrivalDate`)")],
        };
        let converted: ClickhouseChangesError = ApplyOperationError::Wait(wait).into();

        assert!(matches!(converted, ClickhouseChangesError::MutationWait(_)));
        assert!(converted.to_string().contains("0000002215"));
    }

    #[test]
    fn execution_failures_convert_back_to_the_original_error() {
        let converted: ClickhouseChangesError =
            ApplyOperationError::Execute(ClickhouseChangesError::NotSupported("nope".to_string()))
                .into();

        assert!(matches!(converted, ClickhouseChangesError::NotSupported(_)));
    }

    #[tokio::test]
    async fn unrelated_failures_are_not_retried() {
        let executor = AlwaysFailingExecutor::new();

        let error = apply_operation_with_mutation_barrier(
            &NoMutations,
            &executor,
            &modify_column_op("reservations", None),
            "local",
            &fast_config(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ApplyOperationError::Execute(_)));
        assert_eq!(executor.attempts(), 1);
    }
}
