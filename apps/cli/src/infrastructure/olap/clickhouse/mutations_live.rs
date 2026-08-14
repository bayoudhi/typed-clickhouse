//! Live-server tests for the mutation barrier.
//!
//! Everything in [`super::mutations`] other than these tests is covered by
//! scripted fakes. These tests instead exercise the pieces that only a real
//! server can validate: that [`super::mutations::PENDING_MUTATIONS_QUERY`]
//! binds its parameters and deserializes `system.mutations` rows, and that the
//! barrier really blocks until a table's in-flight mutations drain.
//!
//! They are skipped unless `TC_LIVE_CLICKHOUSE=1` is set, so `cargo test`
//! stays hermetic. To run them:
//!
//! ```text
//! docker run -d --name tc-live -p 18123:8123 -p 19000:9000 \
//!     -e CLICKHOUSE_PASSWORD=test123 clickhouse/clickhouse-server:latest
//! TC_LIVE_CLICKHOUSE=1 cargo test live_ -- --test-threads=1
//! ```
//!
//! Connection details default to that container and can be overridden with
//! `TC_LIVE_CH_HOST`, `TC_LIVE_CH_PORT`, `TC_LIVE_CH_USER` and
//! `TC_LIVE_CH_PASSWORD`.
//!
//! # Why the tests stop merges
//!
//! ClickHouse's default `alter_sync = 1` makes `ALTER TABLE ... MODIFY COLUMN`
//! block until its mutation has finished on the current replica, and the CLI
//! does not override it. On a single node that leaves nothing in flight for the
//! barrier to wait on. `SYSTEM STOP MERGES` suspends mutation execution, which
//! reproduces a backlog deterministically and without a large table.

#![cfg(test)]

use std::time::{Duration, Instant};

use super::mutations::{
    run_alter_with_mutation_barrier, AlterRetry, ClientStatementRunner, MutationQuery,
    MutationWaitConfig,
};
use super::{create_client, run_query, ClickHouseConfig, ConfiguredDBClient};

const DB: &str = "typed_clickhouse_live_test";
const ROWS: u64 = 20_000_000;

/// Number of rows in a `system.mutations` count query.
#[derive(clickhouse::Row, serde::Deserialize)]
struct CountRow {
    count: u64,
}

/// A column type from `system.columns`.
#[derive(clickhouse::Row, serde::Deserialize)]
struct TypeRow {
    r#type: String,
}

/// A unix timestamp in seconds, read from the server's clock.
#[derive(clickhouse::Row, serde::Deserialize)]
struct TimestampRow {
    ts: u32,
}

fn enabled() -> bool {
    std::env::var("TC_LIVE_CLICKHOUSE").as_deref() == Ok("1")
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn config() -> ClickHouseConfig {
    ClickHouseConfig {
        db_name: DB.to_string(),
        user: env_or("TC_LIVE_CH_USER", "default"),
        password: env_or("TC_LIVE_CH_PASSWORD", "test123"),
        use_ssl: false,
        host: env_or("TC_LIVE_CH_HOST", "localhost"),
        host_port: env_or("TC_LIVE_CH_PORT", "18123")
            .parse()
            .expect("bad port"),
        native_port: 19000,
        ..Default::default()
    }
}

/// A client that queues ALTERs instead of waiting for them, so a test can leave
/// a mutation in flight on purpose.
fn async_alter_client() -> ConfiguredDBClient {
    let configured = create_client(config());
    ConfiguredDBClient {
        client: configured
            .client
            .with_option("alter_sync", "0")
            .with_option("mutations_sync", "0"),
        config: configured.config,
    }
}

async fn ddl(client: &ConfiguredDBClient, sql: &str) {
    run_query(sql, client)
        .await
        .unwrap_or_else(|e| panic!("statement failed: {sql}\n{e}"));
}

/// Recreates a table of [`ROWS`] rows whose columns are expensive to rewrite.
async fn fresh_table(client: &ConfiguredDBClient, table: &str) {
    // The test database is named in the connection URL, so it has to be created
    // through a client that is not already pointed at it.
    let bootstrap = create_client(ClickHouseConfig {
        db_name: "default".to_string(),
        ..config()
    });
    ddl(&bootstrap, &format!("CREATE DATABASE IF NOT EXISTS `{DB}`")).await;
    ddl(
        client,
        &format!("DROP TABLE IF EXISTS `{DB}`.`{table}` SYNC"),
    )
    .await;
    ddl(
        client,
        &format!(
            "CREATE TABLE `{DB}`.`{table}` (id UInt64, v1 String, v2 String) \
             ENGINE = MergeTree ORDER BY id"
        ),
    )
    .await;
    ddl(
        client,
        &format!(
            "INSERT INTO `{DB}`.`{table}` \
             SELECT number, toString(number), toString(number) FROM numbers({ROWS})"
        ),
    )
    .await;
}

/// `MODIFY COLUMN v{n}`, which rewrites every part.
fn convert(table: &str, n: u32) -> String {
    format!("ALTER TABLE `{DB}`.`{table}` MODIFY COLUMN `v{n}` LowCardinality(String)")
}

async fn unfinished_mutations(client: &ConfiguredDBClient, table: &str) -> u64 {
    client
        .client
        .query(
            "SELECT count() AS count FROM system.mutations \
             WHERE database = ? AND table = ? AND is_done = 0",
        )
        .bind(DB)
        .bind(table)
        .fetch_one::<CountRow>()
        .await
        .expect("count query failed")
        .count
}

/// When each of the table's mutations was queued, oldest first. Read from the
/// server's clock so it is comparable with [`server_now`].
async fn mutation_create_times(client: &ConfiguredDBClient, table: &str) -> Vec<u32> {
    client
        .client
        .query(
            "SELECT toUnixTimestamp(create_time) AS ts FROM system.mutations \
             WHERE database = ? AND table = ? ORDER BY create_time",
        )
        .bind(DB)
        .bind(table)
        .fetch_all::<TimestampRow>()
        .await
        .expect("mutation timing query failed")
        .into_iter()
        .map(|row| row.ts)
        .collect()
}

/// The server's current unix timestamp, in seconds.
async fn server_now(client: &ConfiguredDBClient) -> u32 {
    client
        .client
        .query("SELECT toUnixTimestamp(now()) AS ts")
        .fetch_one::<TimestampRow>()
        .await
        .expect("clock query failed")
        .ts
}

async fn column_type(client: &ConfiguredDBClient, table: &str, column: &str) -> String {
    client
        .client
        .query(
            "SELECT type FROM system.columns \
             WHERE database = ? AND table = ? AND name = ?",
        )
        .bind(DB)
        .bind(table)
        .bind(column)
        .fetch_one::<TypeRow>()
        .await
        .expect("column type query failed")
        .r#type
}

/// `PENDING_MUTATIONS_QUERY` must bind its parameters and deserialize real
/// `system.mutations` rows.
#[tokio::test]
async fn live_pending_mutations_reads_real_rows() {
    if !enabled() {
        return;
    }
    let table = "pending";
    let client = create_client(config());
    fresh_table(&client, table).await;

    // A table with nothing in flight reads back empty rather than erroring.
    let clear = client
        .pending_mutations(DB, table)
        .await
        .expect("query on clear table failed");
    assert!(clear.is_empty(), "expected no mutations, got {clear:?}");

    ddl(&client, &format!("SYSTEM STOP MERGES `{DB}`.`{table}`")).await;
    ddl(&async_alter_client(), &convert(table, 1)).await;

    let pending = client
        .pending_mutations(DB, table)
        .await
        .expect("query with a mutation in flight failed");
    assert_eq!(
        pending.len(),
        1,
        "expected one pending mutation: {pending:?}"
    );
    let mutation = &pending[0];
    assert!(
        !mutation.mutation_id.is_empty(),
        "mutation_id was not deserialized: {mutation:?}"
    );
    assert!(
        mutation.command.contains("MODIFY COLUMN"),
        "command was not deserialized: {mutation:?}"
    );
    assert!(
        mutation.latest_fail_reason.is_empty(),
        "healthy mutation reported a failure: {mutation:?}"
    );

    // The parameters are bound, not interpolated: a table that does not match
    // is filtered server-side, and a name containing quote characters binds
    // cleanly instead of altering the query or erroring.
    for absent in ["no_such_table", "x' OR 1=1 --"] {
        let rows = client
            .pending_mutations(DB, absent)
            .await
            .unwrap_or_else(|e| panic!("query for `{absent}` failed: {e}"));
        assert!(rows.is_empty(), "`{absent}` matched rows: {rows:?}");
    }

    ddl(&client, &format!("SYSTEM START MERGES `{DB}`.`{table}`")).await;
    ddl(&client, &format!("DROP TABLE `{DB}`.`{table}` SYNC")).await;
}

/// The barrier must block a new ALTER until the table's in-flight mutations
/// have drained, then apply it.
#[tokio::test]
async fn live_barrier_waits_for_in_flight_mutation() {
    if !enabled() {
        return;
    }
    let table = "barrier";
    let client = create_client(config());
    fresh_table(&client, table).await;

    // Leave a mutation in flight that cannot make progress.
    ddl(&client, &format!("SYSTEM STOP MERGES `{DB}`.`{table}`")).await;
    ddl(&async_alter_client(), &convert(table, 1)).await;
    assert_eq!(
        unfinished_mutations(&client, table).await,
        1,
        "expected a mutation to be in flight"
    );

    // Let it drain only after a delay, so a barrier that waits is
    // distinguishable from one that does not.
    const BLOCKED_FOR: Duration = Duration::from_secs(5);
    let (send_released_at, released_at) = tokio::sync::oneshot::channel();
    let releaser = tokio::spawn(async move {
        let client = create_client(config());
        tokio::time::sleep(BLOCKED_FOR).await;
        // Read the clock the mutation's `create_time` comes from, so the two
        // are comparable without host/container clock skew.
        let now = server_now(&client).await;
        ddl(&client, &format!("SYSTEM START MERGES `{DB}`.`{table}`")).await;
        send_released_at.send(now).expect("receiver dropped");
    });

    let runner = ClientStatementRunner::new(&client, table);
    let config = MutationWaitConfig {
        timeout: Duration::from_secs(300),
        poll_interval: Duration::from_millis(250),
        ..Default::default()
    };

    let started = Instant::now();
    run_alter_with_mutation_barrier(
        &client,
        &runner,
        &convert(table, 2),
        DB,
        table,
        &config,
        AlterRetry::Allowed,
    )
    .await
    .expect("barriered ALTER failed");
    let waited = started.elapsed();

    releaser.await.expect("releaser task panicked");

    assert!(
        waited >= BLOCKED_FOR,
        "barrier returned after {waited:?}, so it did not wait for the in-flight mutation"
    );

    // The elapsed time alone does not prove the barrier waited: `alter_sync = 1`
    // would have made the second ALTER block on the stopped merges too. What
    // distinguishes them is *when the second mutation was queued*. The barrier
    // holds the statement back until the first mutation drains, which cannot
    // happen before merges are restarted; without the barrier the second
    // mutation would have been queued immediately, well before that.
    let released_at = released_at.await.expect("releaser never reported");
    let created = mutation_create_times(&client, table).await;
    assert_eq!(created.len(), 2, "expected exactly two mutations");
    assert!(
        created[1] >= released_at,
        "second mutation was queued at {} but merges only restarted at {}, \
         so the barrier did not hold the statement back",
        created[1],
        released_at
    );

    assert_eq!(
        unfinished_mutations(&client, table).await,
        0,
        "mutations were still in flight after the barrier returned"
    );
    for column in ["v1", "v2"] {
        assert_eq!(
            column_type(&client, table, column).await,
            "LowCardinality(String)",
            "`{column}` was not converted"
        );
    }

    ddl(&client, &format!("DROP TABLE `{DB}`.`{table}` SYNC")).await;
}
