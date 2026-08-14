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
//! This module provides the barrier that prevents it. The barrier is applied
//! around **each individual `ALTER TABLE` statement**, not around a whole
//! migration operation: a single operation can issue several back-to-back
//! ALTERs against the same table (`ModifyTableColumn` emits up to four), and
//! those need serialising against each other just as much as two separate
//! operations do. Retrying at statement granularity also means a retry never
//! re-executes a statement that already succeeded.
//!
//! The barrier is advisory. If `system.mutations` cannot be read — a transient
//! blip, or a service account without `SELECT` on it — the wait is skipped with
//! a warning rather than failing the migration, and the `CANNOT_ASSIGN_ALTER`
//! retry provides the remaining protection.

use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use super::{ClickhouseChangesError, ConfiguredDBClient};

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

/// How many times to re-issue a statement ClickHouse rejected with
/// `CANNOT_ASSIGN_ALTER`. Each retry re-runs the wait barrier first.
pub const DEFAULT_ALTER_CONFLICT_RETRIES: u32 = 5;

/// How many consecutive polls a mutation must report a non-empty
/// `latest_fail_reason` before the barrier gives up on it.
///
/// A permanently failing mutation never leaves `system.mutations` with
/// `is_done = 0`, so without this the barrier would burn its entire timeout
/// while holding the migration lock. A `latest_fail_reason` can also be set
/// transiently and then clear once ClickHouse retries the mutation
/// successfully, so a single observation must not abort the wait — three
/// consecutive observations is roughly 6s at the default poll interval.
pub const MUTATION_FAILURE_TOLERANCE: u32 = 3;

/// Tuning for the mutation barrier applied around each ALTER statement.
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

    /// A mutation kept reporting a failure and will never drain on its own.
    #[error(
        "in-flight mutation(s) on `{database}`.`{table}` are failing and will not finish:\n{}",
        format_pending(.pending)
    )]
    Failed {
        /// Database of the table whose mutations are failing.
        database: String,
        /// Table whose mutations are failing.
        table: String,
        /// The mutations that kept reporting a failure reason.
        pending: Vec<PendingMutation>,
    },
}

/// Error returned by [`run_alter_with_mutation_barrier`].
#[derive(Debug, thiserror::Error)]
pub enum ApplyOperationError {
    /// Waiting for the table's mutations to drain failed.
    #[error(transparent)]
    Wait(#[from] MutationWaitError),

    /// The statement itself failed.
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

/// Executes a single SQL statement.
#[async_trait]
pub trait StatementRunner: Send + Sync {
    /// Runs `statement` against the database.
    async fn run(&self, statement: &str) -> Result<(), ClickhouseChangesError>;
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

/// Runs statements against a live ClickHouse connection, attributing failures
/// to the table being altered.
pub struct ClientStatementRunner<'a> {
    client: &'a ConfiguredDBClient,
    table: &'a str,
}

impl<'a> ClientStatementRunner<'a> {
    /// Creates a runner bound to a connection and a target table.
    ///
    /// # Arguments
    /// * `client` - The connection to run statements on
    /// * `table` - Table reported as the failing resource on error
    pub fn new(client: &'a ConfiguredDBClient, table: &'a str) -> Self {
        Self { client, table }
    }
}

#[async_trait]
impl StatementRunner for ClientStatementRunner<'_> {
    async fn run(&self, statement: &str) -> Result<(), ClickhouseChangesError> {
        super::run_query(statement, self.client)
            .await
            .map_err(|error| ClickhouseChangesError::ClickhouseClient {
                error,
                resource: Some(self.table.to_string()),
            })
    }
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
/// Returns immediately when the table is already clear. The barrier is
/// advisory: if `system.mutations` cannot be read the wait is skipped with a
/// warning and `Ok(())` is returned, leaving the `CANNOT_ASSIGN_ALTER` retry as
/// the remaining protection.
///
/// Waiting is abandoned early when a mutation reports a non-empty
/// `latest_fail_reason` on [`MUTATION_FAILURE_TOLERANCE`] consecutive polls,
/// because such a mutation never finishes and would otherwise consume the whole
/// timeout.
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
    let mut failing_polls: HashMap<String, u32> = HashMap::new();

    loop {
        let pending = match query.pending_mutations(database, table).await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(
                    "could not read system.mutations for `{}`.`{}`; proceeding without the barrier: {}",
                    database,
                    table,
                    error
                );
                return Ok(());
            }
        };

        if pending.is_empty() {
            if announced {
                tracing::info!("in-flight mutations on '{database}.{table}' finished");
            }
            return Ok(());
        }

        // Track, across polls, which mutations keep reporting a failure.
        failing_polls.retain(|id, _| pending.iter().any(|m| &m.mutation_id == id));
        for mutation in &pending {
            if mutation.latest_fail_reason.is_empty() {
                failing_polls.remove(&mutation.mutation_id);
            } else {
                *failing_polls
                    .entry(mutation.mutation_id.clone())
                    .or_insert(0) += 1;
            }
        }

        let stuck: Vec<PendingMutation> = pending
            .iter()
            .filter(|m| {
                failing_polls
                    .get(&m.mutation_id)
                    .is_some_and(|count| *count >= MUTATION_FAILURE_TOLERANCE)
            })
            .cloned()
            .collect();
        if !stuck.is_empty() {
            return Err(MutationWaitError::Failed {
                database: database.to_string(),
                table: table.to_string(),
                pending: stuck,
            });
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
            tracing::info!(
                "waiting for {} in-flight mutation(s) on '{database}.{table}'...",
                pending.len()
            );
        }

        tokio::time::sleep(config.poll_interval).await;
    }
}

/// Runs a single `ALTER TABLE` statement, serialising it against the target
/// table's in-flight mutations and retrying ClickHouse's retryable alter
/// conflict.
///
/// Only the supplied statement is retried, so an operation that issues several
/// statements never re-executes one that already succeeded.
///
/// # Arguments
/// * `query` - Source of `system.mutations` rows
/// * `runner` - Runs the statement
/// * `statement` - The single SQL statement to run
/// * `database` - Database of the table being altered
/// * `table` - Table being altered
/// * `config` - Wait and retry tuning
pub async fn run_alter_with_mutation_barrier<Q, R>(
    query: &Q,
    runner: &R,
    statement: &str,
    database: &str,
    table: &str,
    config: &MutationWaitConfig,
) -> Result<(), ApplyOperationError>
where
    Q: MutationQuery + ?Sized,
    R: StatementRunner + ?Sized,
{
    let mut attempt = 0;
    loop {
        wait_for_mutations(query, database, table, config).await?;

        match runner.run(statement).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                if attempt >= config.max_retries || !is_alter_conflict(&error) {
                    return Err(ApplyOperationError::Execute(error));
                }
                attempt += 1;
                tracing::warn!(
                    "ClickHouse rejected the statement with a retryable alter conflict; retrying ({}/{}): {}",
                    attempt,
                    config.max_retries,
                    error
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

    const ALTER: &str =
        "ALTER TABLE `local`.`reservations` MODIFY COLUMN `arrivalDate` DateTime64(3)";

    fn pending(id: &str, command: &str) -> PendingMutation {
        PendingMutation {
            mutation_id: id.to_string(),
            command: command.to_string(),
            latest_fail_reason: String::new(),
        }
    }

    fn failing(id: &str, command: &str, reason: &str) -> PendingMutation {
        PendingMutation {
            mutation_id: id.to_string(),
            command: command.to_string(),
            latest_fail_reason: reason.to_string(),
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
            Self::new(vec![vec![mutation]])
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

    /// `system.mutations` cannot be read at all, e.g. missing SELECT grant.
    struct UnreadableMutations {
        polls: Mutex<u32>,
    }

    impl UnreadableMutations {
        fn new() -> Self {
            Self {
                polls: Mutex::new(0),
            }
        }

        fn polls(&self) -> u32 {
            *self.polls.lock().unwrap()
        }
    }

    #[async_trait]
    impl MutationQuery for UnreadableMutations {
        async fn pending_mutations(
            &self,
            database: &str,
            table: &str,
        ) -> Result<Vec<PendingMutation>, MutationQueryError> {
            *self.polls.lock().unwrap() += 1;
            Err(MutationQueryError {
                database: database.to_string(),
                table: table.to_string(),
                source: "Code: 497. DB::Exception: Not enough privileges".into(),
            })
        }
    }

    /// Fails with a `CANNOT_ASSIGN_ALTER` response a fixed number of times, and
    /// records every statement it was asked to run.
    struct ConflictingRunner {
        failures_remaining: Mutex<u32>,
        statements: Mutex<Vec<String>>,
    }

    impl ConflictingRunner {
        fn new(failures: u32) -> Self {
            Self {
                failures_remaining: Mutex::new(failures),
                statements: Mutex::new(Vec::new()),
            }
        }

        fn attempts(&self) -> u32 {
            self.statements.lock().unwrap().len() as u32
        }

        fn statements(&self) -> Vec<String> {
            self.statements.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl StatementRunner for ConflictingRunner {
        async fn run(&self, statement: &str) -> Result<(), ClickhouseChangesError> {
            self.statements.lock().unwrap().push(statement.to_string());
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
    struct AlwaysFailingRunner {
        attempts: Mutex<u32>,
    }

    impl AlwaysFailingRunner {
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
    impl StatementRunner for AlwaysFailingRunner {
        async fn run(&self, _statement: &str) -> Result<(), ClickhouseChangesError> {
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
    fn alter_conflict_is_detected_by_its_symbolic_name_alone() {
        let error = ClickhouseChangesError::NotSupported(
            "DB::Exception: (CANNOT_ASSIGN_ALTER)".to_string(),
        );
        assert!(is_alter_conflict(&error));
    }

    #[test]
    fn alter_conflict_is_detected_through_a_nested_source() {
        let inner = MutationQueryError {
            database: "local".to_string(),
            table: "reservations".to_string(),
            source: "Code: 517. (CANNOT_ASSIGN_ALTER)".into(),
        };
        let outer = ApplyOperationError::Wait(MutationWaitError::Failed {
            database: "local".to_string(),
            table: "reservations".to_string(),
            pending: vec![],
        });

        assert!(is_alter_conflict(&inner));
        // The outer error's own message carries no conflict marker.
        assert!(!is_alter_conflict(&outer));
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

        assert!(matches!(error, MutationWaitError::Timeout { .. }));
        let message = error.to_string();
        assert!(message.contains("0000002215"), "got: {message}");
        assert!(message.contains("`local`.`reservations`"), "got: {message}");
    }

    #[tokio::test]
    async fn an_unreadable_system_mutations_does_not_abort_the_wait() {
        let query = UnreadableMutations::new();

        wait_for_mutations(&query, "local", "reservations", &fast_config())
            .await
            .unwrap();

        assert_eq!(query.polls(), 1, "the barrier must not retry the read");
    }

    #[tokio::test]
    async fn a_statement_still_runs_when_system_mutations_is_unreadable() {
        let query = UnreadableMutations::new();
        let runner = ConflictingRunner::new(0);

        run_alter_with_mutation_barrier(
            &query,
            &runner,
            ALTER,
            "local",
            "reservations",
            &fast_config(),
        )
        .await
        .unwrap();

        assert_eq!(runner.statements(), vec![ALTER.to_string()]);
    }

    #[tokio::test]
    async fn a_persistently_failing_mutation_stops_the_wait_early() {
        let started = Instant::now();
        let query = ScriptedMutations::always_pending(failing(
            "0000002216",
            "(MODIFY COLUMN `arrivalDate` DateTime64(3))",
            "Cannot parse string as DateTime64",
        ));
        let config = MutationWaitConfig {
            timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(1),
            max_retries: 3,
        };

        let error = wait_for_mutations(&query, "local", "reservations", &config)
            .await
            .unwrap_err();

        assert!(matches!(error, MutationWaitError::Failed { .. }));
        assert_eq!(
            query.poll_count(),
            MUTATION_FAILURE_TOLERANCE as usize,
            "must give up on the poll that reaches the tolerance"
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "must not wait out the full timeout"
        );

        let message = error.to_string();
        assert!(message.contains("0000002216"), "got: {message}");
        assert!(
            message.contains("Cannot parse string as DateTime64"),
            "got: {message}"
        );
    }

    #[tokio::test]
    async fn a_transient_failure_reason_does_not_abort_the_wait() {
        let command = "(MODIFY COLUMN `arrivalDate` DateTime64(3))";
        let query = ScriptedMutations::new(vec![
            vec![failing("0000002217", command, "temporary hiccup")],
            vec![failing("0000002217", command, "temporary hiccup")],
            // The fail reason clears: ClickHouse retried the mutation.
            vec![pending("0000002217", command)],
            vec![failing("0000002217", command, "temporary hiccup")],
            vec![],
        ]);

        wait_for_mutations(&query, "local", "reservations", &fast_config())
            .await
            .unwrap();

        assert_eq!(query.poll_count(), 5);
    }

    #[tokio::test]
    async fn a_statement_runs_immediately_when_the_table_is_clear() {
        let query = ScriptedMutations::new(vec![vec![]]);
        let runner = ConflictingRunner::new(0);

        run_alter_with_mutation_barrier(
            &query,
            &runner,
            ALTER,
            "local",
            "reservations",
            &fast_config(),
        )
        .await
        .unwrap();

        assert_eq!(query.poll_count(), 1);
        assert_eq!(runner.statements(), vec![ALTER.to_string()]);
    }

    #[tokio::test]
    async fn a_statement_waits_for_pending_mutations_to_drain_first() {
        let query = ScriptedMutations::new(vec![
            vec![pending(
                "0000002214",
                "(DROP PROJECTION IF EXISTS p_by_unit)",
            )],
            vec![],
        ]);
        let runner = ConflictingRunner::new(0);

        run_alter_with_mutation_barrier(
            &query,
            &runner,
            ALTER,
            "local",
            "reservations",
            &fast_config(),
        )
        .await
        .unwrap();

        assert_eq!(query.poll_count(), 2);
        assert_eq!(runner.attempts(), 1);
    }

    #[tokio::test]
    async fn alter_conflicts_retry_only_the_failing_statement() {
        let runner = ConflictingRunner::new(2);

        run_alter_with_mutation_barrier(
            &NoMutations,
            &runner,
            ALTER,
            "local",
            "reservations",
            &fast_config(),
        )
        .await
        .unwrap();

        assert_eq!(
            runner.statements(),
            vec![ALTER.to_string(), ALTER.to_string(), ALTER.to_string()],
            "the retry re-runs only the statement that was rejected"
        );
    }

    #[tokio::test]
    async fn alter_conflicts_stop_after_the_retry_budget() {
        let runner = ConflictingRunner::new(u32::MAX);

        let error = run_alter_with_mutation_barrier(
            &NoMutations,
            &runner,
            ALTER,
            "local",
            "reservations",
            &fast_config(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ApplyOperationError::Execute(_)));
        assert!(
            is_alter_conflict(&error),
            "the last error is returned unchanged"
        );
        assert_eq!(runner.attempts(), 4, "one initial attempt plus 3 retries");
    }

    #[tokio::test]
    async fn unrelated_failures_are_not_retried() {
        let runner = AlwaysFailingRunner::new();

        let error = run_alter_with_mutation_barrier(
            &NoMutations,
            &runner,
            ALTER,
            "local",
            "reservations",
            &fast_config(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ApplyOperationError::Execute(_)));
        assert_eq!(runner.attempts(), 1);
    }

    #[tokio::test]
    async fn a_wait_failure_is_reported_without_running_the_statement() {
        let query = ScriptedMutations::always_pending(pending("0000002215", "(MODIFY COLUMN `x`)"));
        let runner = ConflictingRunner::new(0);

        let error = run_alter_with_mutation_barrier(
            &query,
            &runner,
            ALTER,
            "local",
            "reservations",
            &fast_config(),
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            ApplyOperationError::Wait(MutationWaitError::Timeout { .. })
        ));
        assert_eq!(runner.attempts(), 0);
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
}
