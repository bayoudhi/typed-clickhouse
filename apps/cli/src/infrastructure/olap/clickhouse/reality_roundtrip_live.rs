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

use super::diff_strategy::ClickHouseTableDiffStrategy;
use super::queries::ClickhouseEngine;
use super::{create_client, run_query, ClickHouseConfig};
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

fn empty_map() -> InfrastructureMap {
    InfrastructureMap {
        default_database: DB.to_string(),
        ..Default::default()
    }
}

/// The synthetic schema. Each carrier triggers exactly one defect:
///
/// - `recordedAt`, `ingestedAt`: `stringDate` annotation (defect 1)
/// - `recordedAt`, `_version`: bare `Delta` codec on 8-byte types (defect 2)
/// - `samples`: nullable field inside a named tuple (defect 3)
/// - `v_device_activity`: declared source tables differ from the SQL (defect 4)
/// - `v_readings_in_window`: control; must never appear
/// - `p_by_device`: references modified columns, so it cascades from 1 and 2
///
/// Views are left out when `with_views` is false so tables can be created
/// before anything that reads from them.
fn fixture(with_views: bool) -> InfrastructureMap {
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

    if with_views {
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
        ];
        for v in views {
            map.views.insert(v.name.clone(), v);
        }
    }

    map
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
    let filter = ReconciliationFilter::from_infra_map(target);
    let reconciled = reconcile_with_reality(project, target, &filter, create_client(config()))
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
                    if before.annotations != after.annotations {
                        fields.push(format!(
                            "annotations reality={:?} code={:?}",
                            before.annotations, after.annotations
                        ));
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
    let tables_only = fixture(false);
    let full = fixture(true);
    apply(&project, &empty_map(), &tables_only).await;
    apply(&project, &tables_only, &full).await;

    let changes = plan_against_reality(&project, &full).await;
    let phantoms: Vec<String> = changes.olap_changes.iter().map(describe).collect();
    assert!(
        phantoms.is_empty(),
        "planner reported {} phantom change(s) against a database it just created:\n{}",
        phantoms.len(),
        phantoms.join("\n")
    );
}
