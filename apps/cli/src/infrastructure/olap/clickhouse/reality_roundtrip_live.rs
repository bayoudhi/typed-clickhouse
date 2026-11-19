//! Live-server round-trip harness for the planner.
//!
//! Builds a fixture schema in a real ClickHouse through the production
//! executor, reads it back through the production reader, and plans against
//! the same fixture. The database was just created from that fixture, so every
//! operation the planner reports is a phantom: a difference between the
//! code-side model and what ClickHouse reports back that no DDL can remove.
//!
//! Skipped unless `TC_LIVE_CLICKHOUSE=1` is set, so `cargo test` stays
//! hermetic. To run it:
//!
//! ```text
//! docker compose -f docker-compose.test.yml up -d --wait
//! TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1
//! ```
//!
//! Connection details default to that container and can be overridden with
//! `TC_LIVE_CH_HOST`, `TC_LIVE_CH_PORT`, `TC_LIVE_CH_USER` and
//! `TC_LIVE_CH_PASSWORD`, as in `mutations_live.rs`.

#![cfg(test)]

use serde_json::json;

use super::diff_strategy::{ddl_relevant_annotations, ClickHouseTableDiffStrategy};
use super::queries::ClickhouseEngine;
use super::{create_client, run_query, ClickHouseConfig};
use crate::framework::core::infrastructure::materialized_view::MaterializedView;
use crate::framework::core::infrastructure::refresh::{
    split_refresh_clause, IntervalUnit, RefreshInterval, RefreshKind, RefreshSchedule,
};
use crate::framework::core::infrastructure::table::{
    Column, ColumnType, IntType, OrderBy, Table, TableProjection,
};
use crate::framework::core::infrastructure::view::View;
use crate::framework::core::infrastructure_map::{
    Change, ColumnChange, InfraChanges, InfrastructureMap, OlapChange, PrimitiveSignature,
    PrimitiveTypes, TableChange,
};
use crate::framework::core::partial_infrastructure_map::LifeCycle;
use crate::framework::core::plan::{
    normalize_infra_map_for_comparison, reconcile_with_reality, ReconciliationFilter,
};
use crate::infrastructure::olap::ddl_ordering::{order_olap_changes, AtomicOlapOperation};
use crate::project::Project;

const DB: &str = "typed_clickhouse_roundtrip_test";

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
            .expect("TC_LIVE_CH_PORT must be a port number"),
        native_port: 19000,
        ..Default::default()
    }
}

fn project() -> Project {
    Project {
        language: crate::framework::languages::SupportedLanguages::Typescript,
        clickhouse_config: config(),
        git_config: crate::utilities::git::GitConfig::default(),
        state_config: crate::project::StateConfig::default(),
        migration_config: crate::project::MigrationConfig::default(),
        language_project_config: crate::project::LanguageProjectConfig::default(),
        project_location: std::path::PathBuf::new(),
        is_production: false,
        log_payloads: false,
        supported_old_versions: std::collections::HashMap::new(),
        jwt: None,
        authentication: crate::project::AuthenticationConfig::default(),
        features: crate::project::ProjectFeatures::default(),
        load_infra: None,
        typescript_config: crate::project::TypescriptConfig::default(),
        source_dir: crate::project::default_source_dir(),
        dev: crate::project::DevConfig::default(),
    }
}

fn column(name: &str, data_type: ColumnType) -> Column {
    Column {
        name: name.to_string(),
        data_type,
        required: true,
        unique: false,
        // ORDER BY columns are not marked primary_key; the reader only sets it
        // for an explicit PRIMARY KEY clause.
        primary_key: false,
        default: None,
        annotations: vec![],
        comment: None,
        ttl: None,
        codec: None,
        materialized: None,
        alias: None,
    }
}

/// What the compiler plugin emits for a `DateTime64String<3>` field. The
/// TypeScript test "marks DateTime64String fields with the stringDate
/// annotation" pins this shape.
fn string_date(name: &str) -> Column {
    Column {
        annotations: vec![("stringDate".to_string(), json!(true))],
        ..column(name, ColumnType::DateTime { precision: Some(3) })
    }
}

fn table(name: &str, columns: Vec<Column>, order_by: &[&str], engine: ClickhouseEngine) -> Table {
    Table {
        name: name.to_string(),
        columns,
        order_by: OrderBy::Fields(order_by.iter().map(|c| c.to_string()).collect()),
        partition_by: None,
        sample_by: None,
        engine,
        version: None,
        source_primitive: PrimitiveSignature {
            name: name.to_string(),
            primitive_type: PrimitiveTypes::DataModel,
        },
        metadata: None,
        life_cycle: LifeCycle::FullyManaged,
        engine_params_hash: None,
        table_settings_hash: None,
        table_settings: None,
        indexes: vec![],
        projections: vec![],
        database: None,
        table_ttl_setting: None,
        cluster_name: None,
        primary_key_expression: None,
        seed_filter: Default::default(),
    }
}

/// How much of the fixture exists; see [`fixture`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Tables,
    Views,
    DependentViews,
}

fn empty_map() -> InfrastructureMap {
    InfrastructureMap {
        default_database: DB.to_string(),
        ..Default::default()
    }
}

/// The synthetic schema. Each carrier triggers exactly one defect:
///
/// - `recordedAt`, `ingestedAt`: `stringDate` annotation (defect 1)
/// - `recordedAt`, `_version`, `sampleTimes`: bare `Delta` codec whose
///   width ClickHouse takes from the stored type, arrays included (defect 2)
/// - `samples`: nullable field inside a named tuple (defect 3)
/// - `v_device_activity`: declared source tables differ from the SQL (defect 4)
/// - `v_readings_in_window`: control; must never appear
/// - `v_device_totals`: upper-case function calls in SQL sqlparser cannot
///   parse; ClickHouse stores the canonical lower-case names (defect 5)
/// - `v_window_devices`: reads a parameterized view as a table function,
///   whose name the formatter quotes on one side only (defect 6)
/// - `mv_device_snapshot`: refreshable MV (`EVERY 1 DAY OFFSET 10 HOUR`) into
///   `device_snapshot`; the clause is stored in `create_table_query`
/// - `mv_device_pairs`: refreshable MV into `device_pairs` whose SELECT
///   (tuple element access, an aliased `JOIN … FINAL`) sqlparser rejects, so
///   it is read back from text alone
/// - `p_by_device`: references modified columns, so it cascades from 1 and 2
///
/// ClickHouse checks that a view's sources exist, so the schema is built in
/// stages: tables, then views over tables, then views over views.
fn fixture(stage: Stage) -> InfrastructureMap {
    let mut map = empty_map();

    let mut readings = table(
        "sensor_readings",
        vec![
            column("deviceId", ColumnType::String),
            Column {
                codec: Some("Delta, ZSTD(1)".to_string()),
                ..string_date("recordedAt")
            },
            Column {
                required: false,
                ..string_date("ingestedAt")
            },
            Column {
                codec: Some("Delta, ZSTD(1)".to_string()),
                ..column("_version", ColumnType::Int(IntType::UInt64))
            },
            Column {
                codec: Some("Delta, ZSTD(1)".to_string()),
                ..column(
                    "sampleTimes",
                    ColumnType::Array {
                        element_type: Box::new(ColumnType::DateTime { precision: Some(3) }),
                        element_nullable: false,
                    },
                )
            },
            column(
                "samples",
                ColumnType::Array {
                    element_type: Box::new(ColumnType::NamedTuple(vec![
                        ("label".to_string(), ColumnType::String),
                        (
                            "ok".to_string(),
                            ColumnType::Nullable(Box::new(ColumnType::Boolean)),
                        ),
                    ])),
                    element_nullable: false,
                },
            ),
        ],
        &["deviceId", "recordedAt"],
        ClickhouseEngine::MergeTree,
    );
    readings.projections = vec![TableProjection {
        name: "p_by_device".to_string(),
        body: "SELECT deviceId, recordedAt, _version ORDER BY _version, deviceId".to_string(),
    }];

    let devices = table(
        "devices",
        vec![
            column("id", ColumnType::String),
            column("label", ColumnType::String),
        ],
        &["id"],
        // FINAL needs an engine that deduplicates.
        ClickhouseEngine::ReplacingMergeTree {
            ver: None,
            is_deleted: None,
        },
    );

    for t in [readings, devices] {
        map.tables.insert(t.id(DB), t);
    }

    if stage >= Stage::Views {
        let snapshot = table(
            "device_snapshot",
            vec![
                column("id", ColumnType::String),
                column("readings", ColumnType::Int(IntType::UInt64)),
            ],
            &["id"],
            ClickhouseEngine::ReplacingMergeTree {
                ver: None,
                is_deleted: None,
            },
        );
        map.tables.insert(snapshot.id(DB), snapshot);

        let mv = snapshot_view(
            "SELECT d.id AS id, count(r.deviceId) AS readings \
             FROM devices AS d \
             LEFT JOIN sensor_readings AS r ON d.id = r.deviceId \
             GROUP BY d.id",
        );
        map.materialized_views.insert(mv.name.clone(), mv);

        let pairs = table(
            "device_pairs",
            vec![
                column("id", ColumnType::String),
                column("label", ColumnType::String),
            ],
            &["id"],
            ClickhouseEngine::MergeTree,
        );
        map.tables.insert(pairs.id(DB), pairs);
        let mv = pairs_view(&format!(
            "SELECT r.deviceId AS id, tuple(d.id, d.label).2 AS label \
             FROM {DB}.sensor_readings AS r \
             JOIN {DB}.devices AS d FINAL ON r.deviceId = d.id"
        ));
        map.materialized_views.insert(mv.name.clone(), mv);

        let views = [
            View {
                name: "v_device_activity".to_string(),
                database: None,
                select_sql: "SELECT r.deviceId, d.label, count() AS readings \
                             FROM sensor_readings AS r \
                             INNER JOIN devices AS d FINAL ON r.deviceId = d.id \
                             GROUP BY r.deviceId, d.label"
                    .to_string(),
                // Declared the way a user passes `baseTables`: only the
                // primary table, although the SQL also joins `devices`.
                source_tables: vec!["`sensor_readings`".to_string()],
                metadata: None,
            },
            View {
                name: "v_readings_in_window".to_string(),
                database: None,
                select_sql: "SELECT deviceId, recordedAt FROM sensor_readings \
                             WHERE recordedAt >= toDateTime64({from:String}, 3)"
                    .to_string(),
                source_tables: vec!["`sensor_readings`".to_string()],
                metadata: None,
            },
            View {
                name: "v_device_totals".to_string(),
                database: None,
                // A qualified table with an alias and FINAL is valid
                // ClickHouse that sqlparser rejects, so this SQL takes the
                // text-only normalization path.
                select_sql: format!(
                    "SELECT
  d.id,
  COUNT(r.deviceId) AS readings,
  SUM(r._version) AS versions,
  AVG(r._version) AS meanVersion,
  IF(COUNT(r.deviceId) > 0, 'active', 'idle') AS state,
  COALESCE(d.label, '') AS label,
  CAST(SUM(r._version), 'Float64') AS versionsFloat
FROM {DB}.devices d FINAL
LEFT JOIN {DB}.sensor_readings r ON d.id = r.deviceId
GROUP BY d.id, d.label"
                ),
                source_tables: vec!["`devices`".to_string(), "`sensor_readings`".to_string()],
                metadata: None,
            },
        ];
        for v in views {
            map.views.insert(v.name.clone(), v);
        }
    }

    if stage >= Stage::DependentViews {
        let view = View {
            name: "v_window_devices".to_string(),
            database: None,
            select_sql: format!(
                "SELECT d.id, d.label, count() AS readings
FROM {DB}.devices d FINAL
JOIN {DB}.v_readings_in_window(from = {{from:String}}) AS w ON d.id = w.deviceId
GROUP BY d.id, d.label"
            ),
            source_tables: vec![
                "`devices`".to_string(),
                "`v_readings_in_window`".to_string(),
            ],
            metadata: None,
        };
        map.views.insert(view.name.clone(), view);
    }

    map
}

fn every(value: u64, unit: IntervalUnit) -> RefreshInterval {
    RefreshInterval { value, unit }
}

/// The refreshable view of the fixture, over the given SELECT.
fn snapshot_view(select_sql: &str) -> MaterializedView {
    MaterializedView {
        name: "mv_device_snapshot".to_string(),
        database: None,
        select_sql: select_sql.to_string(),
        source_tables: vec!["`devices`".to_string(), "`sensor_readings`".to_string()],
        target_table: "device_snapshot".to_string(),
        target_database: None,
        metadata: None,
        life_cycle: LifeCycle::FullyManaged,
        refresh: Some(RefreshSchedule {
            kind: RefreshKind::Every,
            interval: every(1, IntervalUnit::Day),
            offset: Some(every(10, IntervalUnit::Hour)),
            randomize_for: None,
            append: false,
            unmodelled: None,
        }),
    }
}

/// The refreshable view of the fixture whose SELECT sqlparser rejects.
fn pairs_view(select_sql: &str) -> MaterializedView {
    MaterializedView {
        name: "mv_device_pairs".to_string(),
        source_tables: vec!["`sensor_readings`".to_string(), "`devices`".to_string()],
        target_table: "device_pairs".to_string(),
        refresh: Some(RefreshSchedule {
            kind: RefreshKind::Every,
            interval: every(1, IntervalUnit::Hour),
            offset: None,
            randomize_for: None,
            append: false,
            unmodelled: None,
        }),
        ..snapshot_view(select_sql)
    }
}

/// A copy of `map` with the snapshot view changed by `f`.
fn with_snapshot_view(
    map: &InfrastructureMap,
    f: impl FnOnce(&mut MaterializedView),
) -> InfrastructureMap {
    let mut copy = map.clone();
    f(copy
        .materialized_views
        .get_mut("mv_device_snapshot")
        .expect("fixture has the snapshot view"));
    copy
}

#[derive(clickhouse::Row, serde::Deserialize)]
struct CountRow {
    count: u64,
}

#[derive(clickhouse::Row, serde::Deserialize)]
struct TextRow {
    text: String,
}

async fn count_rows(sql: &str) -> u64 {
    create_client(config())
        .client
        .query(sql)
        .fetch_one::<CountRow>()
        .await
        .unwrap_or_else(|e| panic!("query failed: {sql}\n{e}"))
        .count
}

async fn text_of(sql: &str) -> String {
    create_client(config())
        .client
        .query(sql)
        .fetch_one::<TextRow>()
        .await
        .unwrap_or_else(|e| panic!("query failed: {sql}\n{e}"))
        .text
}

async fn create_table_query(name: &str) -> String {
    text_of(&format!(
        "SELECT create_table_query AS text FROM system.tables \
         WHERE database = '{DB}' AND name = '{name}'"
    ))
    .await
}

async fn exec(sql: &str) {
    run_query(sql, &create_client(config()))
        .await
        .unwrap_or_else(|e| panic!("statement failed: {sql}\n{e}"));
}

/// Builds and applies the whole fixture on a fresh database.
async fn apply_full_fixture(project: &Project) -> InfrastructureMap {
    reset_database().await;
    let tables = fixture(Stage::Tables);
    let views = fixture(Stage::Views);
    let full = fixture(Stage::DependentViews);
    apply(project, &empty_map(), &tables).await;
    apply(project, &tables, &views).await;
    apply(project, &views, &full).await;
    full
}

/// Starts from an empty database. The executor checks readiness through a
/// client pointed at the database, so, as in a real deployment, it must exist
/// before anything is applied.
async fn reset_database() {
    // The test database is named in the connection config, so it has to be
    // dropped and created through a client that is not pointed at it.
    let bootstrap = create_client(ClickHouseConfig {
        db_name: "default".to_string(),
        ..config()
    });
    for sql in [
        format!("DROP DATABASE IF EXISTS `{DB}` SYNC"),
        format!("CREATE DATABASE `{DB}`"),
    ] {
        run_query(&sql, &bootstrap)
            .await
            .unwrap_or_else(|e| panic!("statement failed: {sql}\n{e}"));
    }
}

/// Applies `to` over `from` with the executor `migrate` uses.
async fn apply(project: &Project, from: &InfrastructureMap, to: &InfrastructureMap) {
    let changes = from.diff_with_table_strategy(to, &ClickHouseTableDiffStrategy, true, false, &[]);
    crate::infrastructure::olap::execute_changes(project, &changes.olap_changes)
        .await
        .unwrap_or_else(|e| panic!("applying the fixture failed: {e:?}"));
}

/// Plans `target` against the live database the way `plan_changes` does, with
/// `target` standing in for the state a previous deploy of it would have stored.
async fn plan_against_reality(project: &Project, target: &InfrastructureMap) -> InfraChanges {
    plan_from_state(project, target, target).await
}

/// Plans `target` against the live database the way `plan_changes` does, with
/// `stored` as the state a previous deploy stored.
async fn plan_from_state(
    project: &Project,
    stored: &InfrastructureMap,
    target: &InfrastructureMap,
) -> InfraChanges {
    let filter = ReconciliationFilter::from_infra_map(target);
    let reconciled = reconcile_with_reality(project, stored, &filter, create_client(config()))
        .await
        .unwrap_or_else(|e| panic!("reconciling with reality failed: {e:?}"));
    let normalizer = create_client(config());
    let current = normalize_infra_map_for_comparison(&reconciled, &normalizer).await;
    let desired = normalize_infra_map_for_comparison(target, &normalizer).await;
    current.diff_with_table_strategy(&desired, &ClickHouseTableDiffStrategy, true, false, &[])
}

/// One line per phantom, naming the carrier and the field that differs.
fn describe(change: &OlapChange) -> String {
    match change {
        OlapChange::Table(TableChange::Updated {
            name,
            column_changes,
            ..
        }) if !column_changes.is_empty() => column_changes
            .iter()
            .map(|c| match c {
                ColumnChange::Updated { before, after } => {
                    let mut fields = Vec::new();
                    // Only annotations that reach the DDL can drive a change.
                    let (reality, code) = (
                        ddl_relevant_annotations(&before.annotations),
                        ddl_relevant_annotations(&after.annotations),
                    );
                    if reality != code {
                        fields.push(format!("annotations reality={reality:?} code={code:?}"));
                    }
                    if before.codec != after.codec {
                        fields.push(format!(
                            "codec reality={:?} code={:?}",
                            before.codec, after.codec
                        ));
                    }
                    if before.data_type != after.data_type {
                        fields.push(format!(
                            "data_type reality={:?} code={:?}",
                            before.data_type, after.data_type
                        ));
                    }
                    if before.required != after.required {
                        fields.push(format!(
                            "required reality={} code={}",
                            before.required, after.required
                        ));
                    }
                    if fields.is_empty() {
                        fields.push(format!("reality={before:?} code={after:?}"));
                    }
                    format!("{name}.{}: {}", after.name, fields.join("; "))
                }
                other => format!("{name}: {other:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        OlapChange::View(Change::Updated { before, after }) => format!(
            "view {}: select_sql reality={:?} code={:?}; source_tables reality={:?} code={:?}",
            after.name,
            before.select_sql,
            after.select_sql,
            before.source_tables,
            after.source_tables
        ),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn live_reality_roundtrip_plans_nothing() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    reset_database().await;
    let project = project();
    let tables = fixture(Stage::Tables);
    let views = fixture(Stage::Views);
    let full = fixture(Stage::DependentViews);
    apply(&project, &empty_map(), &tables).await;
    apply(&project, &tables, &views).await;
    apply(&project, &views, &full).await;

    let changes = plan_against_reality(&project, &full).await;
    let phantoms: Vec<String> = changes.olap_changes.iter().map(describe).collect();
    assert!(
        phantoms.is_empty(),
        "planner reported {} phantom change(s) against a database it just created:\n{}",
        phantoms.len(),
        phantoms.join("\n")
    );
}

/// Changes the snapshot view's interval on top of `base` (already applied) and
/// checks the change is planned and applied as one MODIFY REFRESH.
async fn assert_schedule_change_is_modify_only(project: &Project, base: &InfrastructureMap) {
    let changed = with_snapshot_view(base, |mv| {
        mv.refresh.as_mut().unwrap().interval = every(2, IntervalUnit::Day);
    });

    let changes = plan_against_reality(project, &changed).await;
    assert!(
        matches!(
            changes.olap_changes.as_slice(),
            [OlapChange::MaterializedView(Change::Updated { .. })]
        ),
        "expected one materialized view update, got: {:?}",
        changes.olap_changes
    );
    let (teardown, setup) = order_olap_changes(&changes.olap_changes, DB).unwrap();
    assert!(teardown.is_empty(), "unexpected teardown: {teardown:?}");
    match setup.as_slice() {
        [AtomicOlapOperation::ModifyMaterializedViewRefresh { mv, .. }] => {
            assert_eq!(mv.name, "mv_device_snapshot");
        }
        other => panic!("expected exactly one ModifyMaterializedViewRefresh, got {other:?}"),
    }

    apply(project, base, &changed).await;

    let after = plan_against_reality(project, &changed).await;
    let phantoms: Vec<String> = after.olap_changes.iter().map(describe).collect();
    assert!(
        phantoms.is_empty(),
        "changes remain after MODIFY REFRESH:\n{}",
        phantoms.join("\n")
    );
    let create = create_table_query("mv_device_snapshot").await;
    assert!(
        create.contains("EVERY 2 DAY"),
        "server did not record the new schedule: {create}"
    );
}

#[tokio::test]
async fn live_refresh_schedule_change_is_modify_only() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    let project = project();
    let full = apply_full_fixture(&project).await;
    assert_schedule_change_is_modify_only(&project, &full).await;
}

#[tokio::test]
async fn live_refresh_schedule_change_on_append_view_is_modify_only() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    let project = project();
    let full = apply_full_fixture(&project).await;
    // Turning APPEND on is a drop and recreate; the schedule change after it
    // must be a MODIFY REFRESH that keeps APPEND, or the server rejects it.
    let appended = with_snapshot_view(&full, |mv| {
        mv.refresh.as_mut().unwrap().append = true;
    });
    apply(&project, &full, &appended).await;
    assert!(
        create_table_query("mv_device_snapshot")
            .await
            .contains(" APPEND TO "),
        "fixture view is not an APPEND view"
    );

    assert_schedule_change_is_modify_only(&project, &appended).await;
    let create = create_table_query("mv_device_snapshot").await;
    assert!(
        create.contains(" APPEND TO "),
        "MODIFY REFRESH dropped APPEND: {create}"
    );
}

#[tokio::test]
async fn live_refresh_schedule_change_on_deletion_protected_view_is_modify_only() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    let project = project();
    let full = apply_full_fixture(&project).await;
    // A lifecycle change alone emits no DDL; the schedule change after it is
    // a MODIFY REFRESH, which deletion protection must let through.
    let protected = with_snapshot_view(&full, |mv| {
        mv.life_cycle = LifeCycle::DeletionProtected;
    });
    apply(&project, &full, &protected).await;
    assert_schedule_change_is_modify_only(&project, &protected).await;
}

#[tokio::test]
async fn live_unmodelled_refresh_is_not_equal_to_plain_schedule() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    let project = project();
    let full = apply_full_fixture(&project).await;
    // Give the deployed view a DEPENDS ON the code does not have. Read back,
    // it must not pass for the code's plain `EVERY 1 DAY OFFSET 10 HOUR`.
    exec(&format!(
        "CREATE TABLE `{DB}`.`upstream_target` (x UInt8) ENGINE = MergeTree ORDER BY x"
    ))
    .await;
    exec(&format!(
        "CREATE MATERIALIZED VIEW `{DB}`.`upstream_mv` REFRESH EVERY 1 DAY OFFSET 10 HOUR \
         TO `{DB}`.`upstream_target` AS SELECT 1 AS x"
    ))
    .await;
    exec(&format!(
        "ALTER TABLE `{DB}`.`mv_device_snapshot` MODIFY REFRESH EVERY 1 DAY OFFSET 10 HOUR \
         DEPENDS ON `{DB}`.`upstream_mv`"
    ))
    .await;
    let create = create_table_query("mv_device_snapshot").await;
    assert!(create.contains("DEPENDS ON"), "fixture: {create}");

    let changes = plan_against_reality(&project, &full).await;
    assert!(
        changes.olap_changes.iter().any(|c| matches!(
            c,
            OlapChange::MaterializedView(Change::Updated { after, .. })
                if after.name == "mv_device_snapshot"
        )),
        "a DEPENDS ON the code lacks was read back as equal; changes: {:?}",
        changes.olap_changes
    );
}

#[tokio::test]
async fn live_refresh_select_change_drops_only_the_view() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    let project = project();
    let full = apply_full_fixture(&project).await;

    exec(&format!(
        "INSERT INTO `{DB}`.`devices` VALUES ('a', 'A'), ('b', 'B')"
    ))
    .await;
    exec(&format!("SYSTEM REFRESH VIEW `{DB}`.`mv_device_snapshot`")).await;
    exec(&format!("SYSTEM WAIT VIEW `{DB}`.`mv_device_snapshot`")).await;
    let before_rows = count_rows(&format!(
        "SELECT count() AS count FROM `{DB}`.`device_snapshot`"
    ))
    .await;
    // Sanity check of the fixture only: every non-APPEND refresh, including
    // the one ClickHouse runs at CREATE, swaps in a new target table and
    // refills it, so neither row counts nor the target's uuid can show that
    // apply left the target alone. The op shape below and the query log do.
    assert!(before_rows > 0, "the refresh did not fill device_snapshot");

    let changed = with_snapshot_view(&full, |mv| {
        mv.select_sql.push_str(" HAVING readings >= 0");
    });
    let changes = plan_against_reality(&project, &changed).await;
    assert!(
        matches!(
            changes.olap_changes.as_slice(),
            [OlapChange::MaterializedView(Change::Updated { .. })]
        ),
        "expected one materialized view update and no table change, got: {:?}",
        changes.olap_changes
    );
    let (teardown, setup) = order_olap_changes(&changes.olap_changes, DB).unwrap();
    assert!(
        matches!(
            teardown.as_slice(),
            [AtomicOlapOperation::DropMaterializedView { mv, .. }] if mv.name == "mv_device_snapshot"
        ),
        "expected only a view drop, got {teardown:?}"
    );
    assert!(
        matches!(
            setup.as_slice(),
            [AtomicOlapOperation::CreateMaterializedView { mv, .. }] if mv.name == "mv_device_snapshot"
        ),
        "expected only a view create, got {setup:?}"
    );

    let started = text_of("SELECT toString(now64(6)) AS text").await;
    apply(&project, &full, &changed).await;

    // Refreshes swap the target internally and log no DROP; a DROP naming the
    // target can only come from the apply.
    exec("SYSTEM FLUSH LOGS").await;
    let target_drops = count_rows(&format!(
        "SELECT count() AS count FROM system.query_log \
         WHERE event_time_microseconds >= toDateTime64('{started}', 6) \
           AND query_kind = 'Drop' AND has(tables, '{DB}.device_snapshot')"
    ))
    .await;
    assert_eq!(target_drops, 0, "apply dropped the target table");
    let view_drops = count_rows(&format!(
        "SELECT count() AS count FROM system.query_log \
         WHERE event_time_microseconds >= toDateTime64('{started}', 6) \
           AND query_kind = 'Drop' AND type = 'QueryFinish' \
           AND has(tables, '{DB}.mv_device_snapshot')"
    ))
    .await;
    assert_eq!(view_drops, 1, "the query log did not record the view drop");
}

#[tokio::test]
async fn live_unparseable_select_round_trips() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    let project = project();
    let full = apply_full_fixture(&project).await;

    // A SELECT change is planned as a drop and recreate of the view only.
    let mut changed = full.clone();
    changed
        .materialized_views
        .get_mut("mv_device_pairs")
        .expect("fixture has the pairs view")
        .select_sql
        .push_str(" WHERE d.label != ''");
    let changes = plan_against_reality(&project, &changed).await;
    let (teardown, setup) = order_olap_changes(&changes.olap_changes, DB).unwrap();
    assert!(
        matches!(
            teardown.as_slice(),
            [AtomicOlapOperation::DropMaterializedView { mv, .. }] if mv.name == "mv_device_pairs"
        ),
        "expected only the view drop, got {teardown:?}"
    );
    assert!(
        matches!(
            setup.as_slice(),
            [AtomicOlapOperation::CreateMaterializedView { mv, .. }] if mv.name == "mv_device_pairs"
        ),
        "expected only the view create, got {setup:?}"
    );

    apply(&project, &full, &changed).await;
    let create = create_table_query("mv_device_pairs").await;
    assert!(
        create.contains("label != ''"),
        "server did not record the new SELECT: {create}"
    );
    let after = plan_against_reality(&project, &changed).await;
    let phantoms: Vec<String> = after.olap_changes.iter().map(describe).collect();
    assert!(
        phantoms.is_empty(),
        "changes remain after recreating the view:\n{}",
        phantoms.join("\n")
    );

    // A schedule-only change is one MODIFY REFRESH, not a drop and recreate.
    let mut rescheduled = changed.clone();
    rescheduled
        .materialized_views
        .get_mut("mv_device_pairs")
        .expect("fixture has the pairs view")
        .refresh
        .as_mut()
        .expect("pairs view is refreshable")
        .interval = every(2, IntervalUnit::Hour);
    let changes = plan_against_reality(&project, &rescheduled).await;
    let (teardown, setup) = order_olap_changes(&changes.olap_changes, DB).unwrap();
    assert!(teardown.is_empty(), "unexpected teardown: {teardown:?}");
    assert!(
        matches!(
            setup.as_slice(),
            [AtomicOlapOperation::ModifyMaterializedViewRefresh { mv, .. }] if mv.name == "mv_device_pairs"
        ),
        "expected exactly one ModifyMaterializedViewRefresh, got {setup:?}"
    );
    apply(&project, &changed, &rescheduled).await;
    let create = create_table_query("mv_device_pairs").await;
    assert!(
        create.contains("EVERY 2 HOUR"),
        "server did not record the new schedule: {create}"
    );
    let after = plan_against_reality(&project, &rescheduled).await;
    let phantoms: Vec<String> = after.olap_changes.iter().map(describe).collect();
    assert!(
        phantoms.is_empty(),
        "changes remain after MODIFY REFRESH:\n{}",
        phantoms.join("\n")
    );
    let changed = rescheduled;

    // Removing the view from code plans its drop.
    let mut removed = changed.clone();
    removed.materialized_views.remove("mv_device_pairs");
    let changes = plan_from_state(&project, &changed, &removed).await;
    let (teardown, setup) = order_olap_changes(&changes.olap_changes, DB).unwrap();
    assert!(
        matches!(
            teardown.as_slice(),
            [AtomicOlapOperation::DropMaterializedView { mv, .. }] if mv.name == "mv_device_pairs"
        ),
        "expected only the view drop, got {teardown:?}"
    );
    assert!(setup.is_empty(), "unexpected setup: {setup:?}");
}

#[tokio::test]
async fn live_mv_without_to_does_not_fail_plan() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    let project = project();
    let full = apply_full_fixture(&project).await;
    exec(&format!(
        "CREATE MATERIALIZED VIEW `{DB}`.`stray` REFRESH EVERY 1 HOUR \
         ENGINE = MergeTree ORDER BY tuple() AS SELECT 1 AS x"
    ))
    .await;

    let changes = plan_against_reality(&project, &full).await;
    let reported: Vec<String> = changes.olap_changes.iter().map(describe).collect();
    assert!(
        reported.is_empty(),
        "planning with a TO-less view present reported changes:\n{}",
        reported.join("\n")
    );
}

#[tokio::test]
async fn live_records_server_spelling() {
    if !enabled() {
        eprintln!("skipping: set TC_LIVE_CLICKHOUSE=1 to run against a live ClickHouse");
        return;
    }

    reset_database().await;
    exec(&format!(
        "CREATE TABLE `{DB}`.`spelling_target` (x UInt8) ENGINE = MergeTree ORDER BY x"
    ))
    .await;
    exec(&format!(
        "CREATE MATERIALIZED VIEW `{DB}`.`spelling_mv` REFRESH EVERY 24 HOUR \
         TO `{DB}`.`spelling_target` AS SELECT 1 AS x"
    ))
    .await;

    let create = create_table_query("spelling_mv").await;
    let (_, parsed) = split_refresh_clause(&create);
    let expected = RefreshSchedule {
        kind: RefreshKind::Every,
        interval: every(24, IntervalUnit::Hour),
        offset: None,
        randomize_for: None,
        append: false,
        unmodelled: None,
    };
    let rewritten = !create.contains("EVERY 24 HOUR");
    assert!(
        parsed.as_ref().is_some_and(|p| p.is_equivalent(&expected)),
        "schedule did not round-trip, got {parsed:?}; server {} `24 HOUR`; create_table_query = {create}",
        if rewritten { "rewrote" } else { "kept" }
    );
    eprintln!(
        "server {} `EVERY 24 HOUR`; create_table_query = {create}",
        if rewritten { "rewrote" } else { "kept" }
    );
}
