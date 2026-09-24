# Phantom Diff Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a plan run against an unchanged, freshly deployed database come back empty, and prove it against a live ClickHouse.

**Architecture:** A live-server test harness builds a synthetic schema in ClickHouse through the production executor, reads it back through the production reader, and plans against the same fixture — so any operation it reports is a phantom by construction. It lands first and fails. Four targeted fixes in the comparison and read-back layers then make it pass: an annotation relevance filter, type-aware codec default widths, tuple-field nullability in the type parser, and views compared by DDL only. A CI job keeps the harness green.

**Tech Stack:** Rust (CLI, `tokio` tests, `clickhouse` 0.14 client), TypeScript (`packages/lib`, mocha + chai), ClickHouse 25.8 via Docker Compose, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-23-phantom-diff-operations-design.md`

## Global Constraints

- Name no external or downstream project anywhere — code, comments, tests, commit messages, PR text. Describe behavior of the diff engine only.
- Fixtures are synthetic: `sensor_readings`, `devices`, `v_device_activity`, `v_readings_in_window`, projection `p_by_device`.
- Live-server tests follow `apps/cli/src/infrastructure/olap/clickhouse/mutations_live.rs`: file named `*_live.rs`, `#![cfg(test)]`, skipped unless `TC_LIVE_CLICKHOUSE=1`, connection overrides `TC_LIVE_CH_HOST` / `TC_LIVE_CH_PORT` / `TC_LIVE_CH_USER` / `TC_LIVE_CH_PASSWORD`, defaults `localhost` / `18123` / `default` / `test123`, test names prefixed `live_`.
- ClickHouse image: `clickhouse/clickhouse-server:25.8`, pinned in `docker-compose.test.yml` only.
- The CLI crate is binary-only. Tests are inline `#[cfg(test)]` modules; nothing goes in `apps/cli/tests/`.
- Before every commit: `cargo fmt`, `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`. For TypeScript changes also `pnpm --filter @typed-clickhouse/lib test`.
- Errors use `thiserror`; never `anyhow::Result`. Constants live in a `constants.rs` at the module level.
- Every commit message ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. A **nullable** `DateTime` column carrying `stringDate` (`required: false`) — expected equal to its read-back; its `Delta` width must see through the `Nullable` wrapper to 8. Pinned in Task 2 (`nullable_string_date_column_is_equivalent`) and Task 3 (`codec_width_sees_through_nullable`), and the harness fixture's `ingestedAt` is nullable.
2. A **LowCardinality column that also carries a code-only annotation** — expected equal to a read-back carrying only `LowCardinality`, and still unequal to a column without `LowCardinality`. Pinned in Task 2 (`ddl_relevant_annotations_ignore_code_side_metadata`).
3. A `stringDate` field **inside a `Nested` column** — `nested_are_equivalent` has its own annotation comparison, expected to ignore it too. Pinned in Task 2 (`nested_string_date_field_is_equivalent`).
4. A nullable tuple field **not directly inside an `Array`** — e.g. a `Map` value tuple; the parser fix must apply in every context the tuple branch is reached. Pinned in Task 4 (`map_value_tuple_fields_keep_nullability`).
5. A view whose **SQL or database genuinely changes** — expected still to be recreated once `source_tables` stops counting. Pinned in Task 5 (`test_views_ignore_declared_source_tables`).

---

### Task 1: Live reproduction harness

Lands the failing reproduction, the compose file it runs against, and a TypeScript guard that pins the one fixture assumption taken from the compiler plugin. The harness is skipped under a plain `cargo test`, so the branch stays green; it fails only when run live.

**Files:**
- Create: `docker-compose.test.yml`
- Create: `apps/cli/src/infrastructure/olap/clickhouse/reality_roundtrip_live.rs`
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/mod.rs:84-85` (register the module)
- Modify: `packages/lib/tests/typeConvert.test.ts` (append one `it`)

**Interfaces:**
- Consumes: `reconcile_with_reality`, `normalize_infra_map_for_comparison`, `ReconciliationFilter` (`apps/cli/src/framework/core/plan.rs`); `InfrastructureMap::diff_with_table_strategy`; `crate::infrastructure::olap::execute_changes(&Project, &[OlapChange])`; `create_client`, `run_query`, `ClickHouseConfig` (`clickhouse/mod.rs`).
- Produces: test `live_reality_roundtrip_plans_nothing`, runnable as `TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1`. Tasks 2–5 use it as their end-to-end check; Task 6 wires it into CI.

- [ ] **Step 1: Add the compose file**

Create `docker-compose.test.yml`:

```yaml
# ClickHouse for the live-server test modules (`*_live.rs` under
# apps/cli/src/infrastructure/olap/clickhouse). Port and password match
# their defaults.
#
#   docker compose -f docker-compose.test.yml up -d --wait
#   TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1
services:
  clickhouse:
    image: clickhouse/clickhouse-server:25.8
    ports:
      - "18123:8123"
      - "19000:9000"
    environment:
      CLICKHOUSE_PASSWORD: test123
    ulimits:
      nofile:
        soft: 262144
        hard: 262144
    healthcheck:
      test: ["CMD", "clickhouse-client", "--password", "test123", "--query", "SELECT 1"]
      interval: 2s
      timeout: 5s
      retries: 30
```

- [ ] **Step 2: Start it and confirm it answers**

Run:
```bash
docker compose -f docker-compose.test.yml up -d --wait
curl -s -u default:test123 'http://localhost:18123/?query=SELECT%20version()'
```
Expected: a version string beginning `25.8`. If Docker reports no space, stop and report — do not prune anything.

- [ ] **Step 3: Register the harness module**

In `apps/cli/src/infrastructure/olap/clickhouse/mod.rs`, directly below the existing

```rust
#[cfg(test)]
mod mutations_live;
```

add:

```rust
#[cfg(test)]
mod reality_roundtrip_live;
```

- [ ] **Step 4: Write the harness**

Create `apps/cli/src/infrastructure/olap/clickhouse/reality_roundtrip_live.rs`:

```rust
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

async fn reset_database() {
    // The test database is named in the connection config, so it has to be
    // dropped through a client that is not pointed at it.
    let bootstrap = create_client(ClickHouseConfig {
        db_name: "default".to_string(),
        ..config()
    });
    let sql = format!("DROP DATABASE IF EXISTS `{DB}` SYNC");
    run_query(&sql, &bootstrap)
        .await
        .unwrap_or_else(|e| panic!("statement failed: {sql}\n{e}"));
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
            after.name, before.select_sql, after.select_sql, before.source_tables, after.source_tables
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
```

- [ ] **Step 5: Confirm the hermetic run skips it**

Run: `cargo test live_reality -- --nocapture 2>&1 | grep -E "skipping|test result"`
Expected: `skipping: set TC_LIVE_CLICKHOUSE=1 ...` and `test result: ok. 1 passed`.

If this fails to compile, the likeliest causes are a `Project` field added since this plan was written (copy the field list from `create_test_project` in `apps/cli/src/framework/core/plan.rs`) or an import path; fix those only.

- [ ] **Step 6: Run it live and confirm it reproduces**

Run: `TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1 --nocapture 2>&1 | tail -30`

Expected: FAIL with `planner reported N phantom change(s)`, and the listed lines include, in some order:

- `sensor_readings.recordedAt:` with both an `annotations` and a `codec` difference (`Delta(8), ZSTD(1)` vs `Delta, ZSTD(1)`)
- `sensor_readings.ingestedAt:` with an `annotations` difference
- `sensor_readings._version:` with a `codec` difference
- `sensor_readings.samples:` with a `data_type` difference in the `ok` field's nullability
- `view v_device_activity:` with equal `select_sql` and different `source_tables`

It must **not** list `v_readings_in_window`. If it lists anything outside these five carriers, stop and report the output — that is a defect outside the spec, not something to fix here.

- [ ] **Step 7: Add the TypeScript fixture guard**

Append inside the `describe("typeConvert mappings for helper types", ...)` block in `packages/lib/tests/typeConvert.test.ts`:

```ts
  it("marks DateTime64String fields with the stringDate annotation", function () {
    // The CLI's live round-trip harness builds its date columns in this
    // shape. If the plugin stops emitting it, update that fixture too.
    const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "tch-typeconv-"));

    const source = `
      import { DateTime64String } from "@514labs/moose-lib";

      export interface TestModel {
        recordedAt: DateTime64String<3>;
      }
    `;

    const { checker, type } = createProgramWithSource(tempDir, source);
    const [recordedAt] = toColumns(type, checker);

    expect(recordedAt.data_type).to.equal("DateTime(3)");
    expect(recordedAt.annotations).to.deep.include(["stringDate", true]);
  });
```

- [ ] **Step 8: Run the TypeScript suite**

Run: `pnpm --filter @typed-clickhouse/lib test 2>&1 | tail -5`
Expected: all passing, including `marks DateTime64String fields with the stringDate annotation`.

- [ ] **Step 9: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
git add docker-compose.test.yml apps/cli/src/infrastructure/olap/clickhouse/reality_roundtrip_live.rs apps/cli/src/infrastructure/olap/clickhouse/mod.rs packages/lib/tests/typeConvert.test.ts
git commit -F - <<'EOF'
test: reproduce phantom plan operations against a live ClickHouse

Adds a live-server harness that creates a synthetic schema through the
production executor, reads it back through the production reader, and
plans against the same fixture. Anything it reports is a phantom by
construction. It fails today on four carriers: code-only annotations,
bare Delta codecs on 8-byte types, tuple-field nullability, and views
whose declared source tables differ from their SQL.

Skipped unless TC_LIVE_CLICKHOUSE=1, following mutations_live.rs. A
TypeScript test pins the stringDate annotation the fixture assumes.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 2: Ignore annotations that do not affect DDL

**Files:**
- Create: `apps/cli/src/infrastructure/olap/clickhouse/constants.rs`
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/mod.rs:74-89` (declare `pub mod constants;`)
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/mapper.rs:157-201` (use the constants)
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/diff_strategy.rs:219` and add `ddl_relevant_annotations`
- Modify: `apps/cli/src/framework/core/infrastructure_map.rs:2441`
- Test: `diff_strategy.rs` `mod tests`, `infrastructure_map.rs` `mod tests`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub const DDL_RELEVANT_ANNOTATIONS: [&str; 3]` in `clickhouse::constants`; `pub fn ddl_relevant_annotations(annotations: &[(String, serde_json::Value)]) -> Vec<&(String, serde_json::Value)>` in `clickhouse::diff_strategy`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `apps/cli/src/infrastructure/olap/clickhouse/diff_strategy.rs`:

```rust
    #[test]
    fn ddl_relevant_annotations_ignore_code_side_metadata() {
        use serde_json::json;

        let code = vec![
            ("LowCardinality".to_string(), json!(true)),
            ("stringDate".to_string(), json!(true)),
        ];
        let reality = vec![("LowCardinality".to_string(), json!(true))];

        assert_eq!(
            ddl_relevant_annotations(&code),
            ddl_relevant_annotations(&reality)
        );
        assert_ne!(ddl_relevant_annotations(&code), ddl_relevant_annotations(&[]));
    }
```

Append to `mod tests` in `apps/cli/src/framework/core/infrastructure_map.rs`:

```rust
    fn date_column(required: bool, annotations: Vec<(String, serde_json::Value)>) -> Column {
        Column {
            name: "recordedAt".to_string(),
            data_type: ColumnType::DateTime { precision: Some(3) },
            required,
            unique: false,
            primary_key: false,
            default: None,
            annotations,
            comment: None,
            ttl: None,
            codec: None,
            materialized: None,
            alias: None,
        }
    }

    fn string_date() -> Vec<(String, serde_json::Value)> {
        vec![("stringDate".to_string(), serde_json::json!(true))]
    }

    #[test]
    fn string_date_annotation_alone_is_not_a_change() {
        assert!(columns_are_equivalent(
            &date_column(true, vec![]),
            &date_column(true, string_date()),
            &[]
        ));
    }

    #[test]
    fn nullable_string_date_column_is_equivalent() {
        assert!(columns_are_equivalent(
            &date_column(false, vec![]),
            &date_column(false, string_date()),
            &[]
        ));
    }

    #[test]
    fn low_cardinality_annotation_is_still_a_change() {
        let plain = Column {
            data_type: ColumnType::String,
            ..date_column(true, vec![])
        };
        let low_cardinality = Column {
            annotations: vec![("LowCardinality".to_string(), serde_json::json!(true))],
            ..plain.clone()
        };
        assert!(!columns_are_equivalent(&plain, &low_cardinality, &[]));
    }

    #[test]
    fn nested_string_date_field_is_equivalent() {
        use crate::framework::core::infrastructure::table::Nested;

        let nested = |annotations| Column {
            data_type: ColumnType::Nested(Nested {
                name: "events".to_string(),
                columns: vec![date_column(true, annotations)],
                jwt: false,
            }),
            ..date_column(true, vec![])
        };
        assert!(columns_are_equivalent(&nested(vec![]), &nested(string_date()), &[]));
    }
```

If `Column` or `ColumnType` is not already in scope in that test module, add `use crate::framework::core::infrastructure::table::{Column, ColumnType};` at its top.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test ddl_relevant_annotations string_date nested_string_date low_cardinality_annotation 2>&1 | tail -20`

Expected: compile error `cannot find function ddl_relevant_annotations`. Temporarily comment out the `diff_strategy.rs` test and rerun to confirm the three `infrastructure_map.rs` equivalence tests FAIL on `assert!` and `low_cardinality_annotation_is_still_a_change` PASSES. Uncomment it afterwards.

- [ ] **Step 3: Add the constants module**

Create `apps/cli/src/infrastructure/olap/clickhouse/constants.rs`:

```rust
//! Annotation keys shared by the DDL renderer and the diff.

/// Wraps a column type in `LowCardinality(...)`.
pub const LOW_CARDINALITY_ANNOTATION: &str = "LowCardinality";

/// Renders a column as `AggregateFunction(...)`.
pub const AGGREGATION_FUNCTION_ANNOTATION: &str = "aggregationFunction";

/// Renders a column as `SimpleAggregateFunction(...)`.
pub const SIMPLE_AGGREGATION_FUNCTION_ANNOTATION: &str = "simpleAggregationFunction";

/// Annotations that change generated DDL: exactly the keys
/// `mapper::std_field_type_to_clickhouse_type_mapper` reads. Every other
/// annotation is code-side metadata ClickHouse cannot store, so it must not
/// decide whether a column changed.
pub const DDL_RELEVANT_ANNOTATIONS: [&str; 3] = [
    LOW_CARDINALITY_ANNOTATION,
    AGGREGATION_FUNCTION_ANNOTATION,
    SIMPLE_AGGREGATION_FUNCTION_ANNOTATION,
];
```

In `apps/cli/src/infrastructure/olap/clickhouse/mod.rs`, add `pub mod constants;` to the alphabetical `pub mod` list so that section reads:

```rust
pub mod config_resolver;
pub mod constants;
pub mod diagnostics;
```

- [ ] **Step 4: Make the renderer read the same constants**

In `apps/cli/src/infrastructure/olap/clickhouse/mapper.rs`, add to the imports:

```rust
use super::constants::{
    AGGREGATION_FUNCTION_ANNOTATION, LOW_CARDINALITY_ANNOTATION,
    SIMPLE_AGGREGATION_FUNCTION_ANNOTATION,
};
```

Then in `std_field_type_to_clickhouse_type_mapper` replace the three string literals:

- `.find(|(k, _)| k == "simpleAggregationFunction")` → `.find(|(k, _)| k == SIMPLE_AGGREGATION_FUNCTION_ANNOTATION)`
- `.find(|(k, _)| k == "aggregationFunction")` → `.find(|(k, _)| k == AGGREGATION_FUNCTION_ANNOTATION)`
- `.any(|(k, v)| k == "LowCardinality" && v == &serde_json::json!(true))` → `.any(|(k, v)| k == LOW_CARDINALITY_ANNOTATION && v == &serde_json::json!(true))`

This is what makes the registry derived from the renderer rather than maintained beside it: both now name the same constants.

- [ ] **Step 5: Add the filter and use it in `nested_are_equivalent`**

In `apps/cli/src/infrastructure/olap/clickhouse/diff_strategy.rs`, add near `normalize_column_for_low_cardinality_ignore`:

```rust
/// The annotations that influence generated DDL, in their original order.
///
/// Code-side metadata such as `stringDate` has no representation in
/// ClickHouse, so it is always absent from a column read back from the
/// database. Comparing it would report a change no DDL can make.
pub fn ddl_relevant_annotations(
    annotations: &[(String, serde_json::Value)],
) -> Vec<&(String, serde_json::Value)> {
    use super::constants::DDL_RELEVANT_ANNOTATIONS;

    annotations
        .iter()
        .filter(|(key, _)| DDL_RELEVANT_ANNOTATIONS.contains(&key.as_str()))
        .collect()
}
```

In `nested_are_equivalent`, replace

```rust
            || normalized_actual.annotations != normalized_target.annotations
```

with

```rust
            || ddl_relevant_annotations(&normalized_actual.annotations)
                != ddl_relevant_annotations(&normalized_target.annotations)
```

- [ ] **Step 6: Use it in `columns_are_equivalent`**

In `apps/cli/src/framework/core/infrastructure_map.rs`, extend the `use` inside `columns_are_equivalent`:

```rust
    use crate::infrastructure::olap::clickhouse::{
        diff_strategy::{
            column_types_are_equivalent, ddl_relevant_annotations,
            normalize_column_for_low_cardinality_ignore,
        },
        IgnorableOperation,
    };
```

and replace

```rust
        || normalized_before.annotations != normalized_after.annotations
```

with

```rust
        || ddl_relevant_annotations(&normalized_before.annotations)
            != ddl_relevant_annotations(&normalized_after.annotations)
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test ddl_relevant_annotations string_date nested_string_date low_cardinality_annotation 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: all five PASS. Then `cargo test 2>&1 | grep "test result"` — every suite `ok`.

- [ ] **Step 8: Confirm the harness moved**

Run: `TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1 --nocapture 2>&1 | tail -20`
Expected: still FAIL, but no line mentions `annotations`. `ingestedAt` is gone entirely; `recordedAt` remains with only its `codec` difference.

- [ ] **Step 9: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add apps/cli/src/infrastructure/olap/clickhouse/constants.rs apps/cli/src/infrastructure/olap/clickhouse/mod.rs apps/cli/src/infrastructure/olap/clickhouse/mapper.rs apps/cli/src/infrastructure/olap/clickhouse/diff_strategy.rs apps/cli/src/framework/core/infrastructure_map.rs
git commit -F - <<'EOF'
fix: ignore annotations that do not affect DDL when diffing columns

Column equivalence compared the full annotation list. Code-side
metadata such as stringDate, which the compiler plugin attaches to
every date-time string field, has no representation in ClickHouse and
is always absent on read-back, so every such column was reported as
modified on every plan.

Only the annotation keys the DDL renderer reads now take part in the
comparison. The renderer and the filter name the same constants, so a
new DDL-affecting annotation cannot be added to one without the other.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 3: Resolve codec default widths from the column type

**Files:**
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/mod.rs:3507-3533` (`normalize_codec_expression`, `codec_expressions_are_equivalent`, new `codec_default_width`)
- Modify: `apps/cli/src/framework/core/infrastructure_map.rs:2454` (caller)
- Test: `apps/cli/src/infrastructure/olap/clickhouse/mod.rs` `mod tests` (rewrite two existing tests, add two)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub fn normalize_codec_expression(expr: &str, data_type: &ColumnType) -> String`; `pub fn codec_expressions_are_equivalent(before: &Option<String>, after: &Option<String>, data_type: &ColumnType) -> bool`. Both signatures gain a parameter; `infrastructure_map.rs:2454` is the only non-test caller.

- [ ] **Step 1: Rewrite the existing tests against the new signature and add the failing ones**

In `apps/cli/src/infrastructure/olap/clickhouse/mod.rs` `mod tests`, replace `test_normalize_codec_expression` and `test_codec_expressions_are_equivalent` in full with:

```rust
    fn uint32() -> ColumnType {
        ColumnType::Int(IntType::UInt32)
    }

    fn float64() -> ColumnType {
        ColumnType::Float(FloatType::Float64)
    }

    #[test]
    fn test_normalize_codec_expression() {
        // Bare codecs gain ClickHouse's defaults
        assert_eq!(normalize_codec_expression("Delta", &uint32()), "Delta(4)");
        assert_eq!(normalize_codec_expression("Gorilla", &float64()), "Gorilla(8)");
        assert_eq!(normalize_codec_expression("ZSTD", &uint32()), "ZSTD(1)");

        // Codecs with params stay as-is
        assert_eq!(normalize_codec_expression("Delta(4)", &uint32()), "Delta(4)");
        assert_eq!(normalize_codec_expression("Gorilla(8)", &float64()), "Gorilla(8)");
        assert_eq!(normalize_codec_expression("ZSTD(3)", &uint32()), "ZSTD(3)");
        assert_eq!(normalize_codec_expression("ZSTD(9)", &uint32()), "ZSTD(9)");

        // Codecs without default params
        assert_eq!(normalize_codec_expression("DoubleDelta", &uint32()), "DoubleDelta");
        assert_eq!(normalize_codec_expression("LZ4", &uint32()), "LZ4");
        assert_eq!(normalize_codec_expression("NONE", &uint32()), "NONE");

        // Chains
        assert_eq!(normalize_codec_expression("Delta, LZ4", &uint32()), "Delta(4), LZ4");
        assert_eq!(
            normalize_codec_expression("Gorilla, ZSTD", &float64()),
            "Gorilla(8), ZSTD(1)"
        );
        assert_eq!(
            normalize_codec_expression("Delta, ZSTD(3)", &uint32()),
            "Delta(4), ZSTD(3)"
        );
        assert_eq!(
            normalize_codec_expression("DoubleDelta, LZ4", &uint32()),
            "DoubleDelta, LZ4"
        );

        // Whitespace
        assert_eq!(normalize_codec_expression("Delta,LZ4", &uint32()), "Delta(4), LZ4");
        assert_eq!(
            normalize_codec_expression("  Delta  ,  LZ4  ", &uint32()),
            "Delta(4), LZ4"
        );

        // Already normalized
        assert_eq!(
            normalize_codec_expression("Delta(4), LZ4", &uint32()),
            "Delta(4), LZ4"
        );
        assert_eq!(
            normalize_codec_expression("Gorilla(8), ZSTD(3)", &float64()),
            "Gorilla(8), ZSTD(3)"
        );
    }

    #[test]
    fn test_codec_expressions_are_equivalent() {
        let t = uint32();
        let some = |s: &str| Some(s.to_string());

        assert!(codec_expressions_are_equivalent(&None, &None, &t));
        assert!(!codec_expressions_are_equivalent(&some("ZSTD(3)"), &None, &t));
        assert!(codec_expressions_are_equivalent(&some("ZSTD(3)"), &some("ZSTD(3)"), &t));
        assert!(codec_expressions_are_equivalent(&some("Delta"), &some("Delta(4)"), &t));
        assert!(codec_expressions_are_equivalent(
            &some("Gorilla"),
            &some("Gorilla(8)"),
            &float64()
        ));
        assert!(codec_expressions_are_equivalent(&some("ZSTD"), &some("ZSTD(1)"), &t));
        assert!(codec_expressions_are_equivalent(
            &some("Delta, LZ4"),
            &some("Delta(4), LZ4"),
            &t
        ));
        assert!(!codec_expressions_are_equivalent(&some("ZSTD(3)"), &some("ZSTD(9)"), &t));
        assert!(!codec_expressions_are_equivalent(
            &some("Delta, LZ4"),
            &some("Delta, ZSTD"),
            &t
        ));
    }

    #[test]
    fn codec_width_follows_the_stored_type() {
        let some = |s: &str| Some(s.to_string());
        let uint64 = ColumnType::Int(IntType::UInt64);
        // DateTime with a precision is stored as the 8-byte DateTime64.
        let datetime64 = ColumnType::DateTime { precision: Some(3) };
        let datetime = ColumnType::DateTime { precision: None };

        assert!(codec_expressions_are_equivalent(
            &some("Delta, ZSTD(1)"),
            &some("Delta(8), ZSTD(1)"),
            &uint64
        ));
        assert!(codec_expressions_are_equivalent(
            &some("Delta,ZSTD(1)"),
            &some("Delta(8), ZSTD(1)"),
            &datetime64
        ));
        assert!(codec_expressions_are_equivalent(
            &some("Delta"),
            &some("Delta(4)"),
            &datetime
        ));
        assert!(codec_expressions_are_equivalent(
            &some("Delta"),
            &some("Delta(2)"),
            &ColumnType::Date16
        ));
        assert!(codec_expressions_are_equivalent(
            &some("Gorilla"),
            &some("Gorilla(4)"),
            &ColumnType::Float(FloatType::Float32)
        ));
        // A width the type does not have is a real difference.
        assert!(!codec_expressions_are_equivalent(
            &some("Delta"),
            &some("Delta(4)"),
            &uint64
        ));
        // Unknown width: leave the codec as written rather than guess.
        assert_eq!(normalize_codec_expression("Delta", &ColumnType::String), "Delta");
        assert!(!codec_expressions_are_equivalent(
            &some("Delta"),
            &some("Delta(8)"),
            &ColumnType::String
        ));
    }

    #[test]
    fn codec_width_sees_through_nullable() {
        let nullable_datetime64 =
            ColumnType::Nullable(Box::new(ColumnType::DateTime { precision: Some(3) }));
        assert!(codec_expressions_are_equivalent(
            &Some("Delta".to_string()),
            &Some("Delta(8)".to_string()),
            &nullable_datetime64
        ));
    }
```

If `IntType` or `FloatType` is not in scope in that module, add `use crate::framework::core::infrastructure::table::{FloatType, IntType};` at the top of `mod tests`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test codec_ 2>&1 | grep -E "error\[|expected .* arguments" | head`
Expected: compile errors — `this function takes 1 argument but 2 arguments were supplied` for `normalize_codec_expression`, and the same shape for `codec_expressions_are_equivalent`.

- [ ] **Step 3: Implement type-aware normalization**

In `apps/cli/src/infrastructure/olap/clickhouse/mod.rs`, replace `normalize_codec_expression` and `codec_expressions_are_equivalent` with:

```rust
/// Byte width ClickHouse substitutes for a bare `Delta` or `Gorilla`: the size
/// of the stored value. `None` when that is not a fixed 1, 2, 4 or 8 bytes.
fn codec_default_width(data_type: &ColumnType) -> Option<u8> {
    use crate::framework::core::infrastructure::table::{FloatType, IntType};

    match data_type {
        ColumnType::Nullable(inner) => codec_default_width(inner),
        ColumnType::Boolean | ColumnType::Int(IntType::Int8 | IntType::UInt8) => Some(1),
        ColumnType::Date16 | ColumnType::Int(IntType::Int16 | IntType::UInt16) => Some(2),
        ColumnType::Date
        | ColumnType::DateTime { precision: None }
        | ColumnType::Float(FloatType::Float32)
        | ColumnType::Int(IntType::Int32 | IntType::UInt32) => Some(4),
        // A DateTime with a precision is stored as DateTime64.
        ColumnType::DateTime { precision: Some(_) }
        | ColumnType::Float(FloatType::Float64)
        | ColumnType::Int(IntType::Int64 | IntType::UInt64) => Some(8),
        _ => None,
    }
}

/// Expands bare codec names to the parameters ClickHouse stores for them.
///
/// `Delta` and `Gorilla` default to the byte width of the column's type, so
/// the same codec reads back as `Delta(4)` on a `UInt32` and `Delta(8)` on a
/// `UInt64`. When the width is unknown the codec is left as written: at worst
/// a difference survives, never a real one hidden.
pub fn normalize_codec_expression(expr: &str, data_type: &ColumnType) -> String {
    let width = codec_default_width(data_type);
    expr.split(',')
        .map(|codec| {
            let trimmed = codec.trim();
            match (trimmed, width) {
                ("Delta" | "Gorilla", Some(width)) => format!("{trimmed}({width})"),
                ("ZSTD", _) => "ZSTD(1)".to_string(),
                // DoubleDelta, LZ4, NONE, codecs with explicit params, and
                // Delta/Gorilla on a type of unknown width stay as written.
                _ => trimmed.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Checks if two codec expressions on a column of `data_type` are
/// semantically equivalent after normalization.
///
/// For example, `Delta, LZ4` from user code is equivalent to `Delta(8), LZ4`
/// from ClickHouse on a `UInt64` column.
pub fn codec_expressions_are_equivalent(
    before: &Option<String>,
    after: &Option<String>,
    data_type: &ColumnType,
) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(b), Some(a)) => {
            normalize_codec_expression(b, data_type) == normalize_codec_expression(a, data_type)
        }
        _ => false,
    }
}
```

- [ ] **Step 4: Update the caller**

In `apps/cli/src/framework/core/infrastructure_map.rs` `columns_are_equivalent`, replace

```rust
    if !codec_expressions_are_equivalent(&before.codec, &after.codec) {
```

with

```rust
    // The codec width depends on the column type. If the types differ, the
    // type comparison below reports the change regardless of this result.
    if !codec_expressions_are_equivalent(&before.codec, &after.codec, &after.data_type) {
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test codec_ 2>&1 | grep -E "test result|FAILED|panicked"`
Expected: all four codec tests PASS. Then `cargo test 2>&1 | grep "test result"` — every suite `ok`.

- [ ] **Step 6: Confirm the harness moved**

Run: `TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1 --nocapture 2>&1 | tail -20`
Expected: still FAIL, listing only `sensor_readings.samples` and `view v_device_activity`.

- [ ] **Step 7: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add apps/cli/src/infrastructure/olap/clickhouse/mod.rs apps/cli/src/framework/core/infrastructure_map.rs
git commit -F - <<'EOF'
fix: resolve bare Delta and Gorilla codec widths from the column type

Codec normalization expanded a bare Delta to Delta(4) and a bare Gorilla
to Gorilla(8) regardless of type. ClickHouse substitutes the byte width
of the stored value, so a Delta on a UInt64 or a DateTime64 column reads
back as Delta(8) and never matched, and the column was re-modified on
every plan.

The width now comes from the column type. Types without a fixed 1, 2, 4
or 8 byte width leave the codec as written rather than guessing.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 4: Keep nullability on named tuple fields

**Files:**
- Modify: `apps/cli/src/infrastructure/olap/clickhouse/type_parser.rs:1712-1727`
- Test: `apps/cli/src/infrastructure/olap/clickhouse/type_parser.rs` `mod tests`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `convert_clickhouse_type_to_column_type` now returns `ColumnType::Nullable(..)` for nullable named-tuple fields. No signature change.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `type_parser.rs`:

```rust
    #[test]
    fn named_tuple_fields_keep_nullability() {
        let (column_type, is_nullable) = convert_clickhouse_type_to_column_type(
            "Array(Tuple(label String, ok Nullable(Bool)))",
        )
        .unwrap();

        assert!(!is_nullable);
        assert_eq!(
            column_type,
            ColumnType::Array {
                element_type: Box::new(ColumnType::NamedTuple(vec![
                    ("label".to_string(), ColumnType::String),
                    (
                        "ok".to_string(),
                        ColumnType::Nullable(Box::new(ColumnType::Boolean))
                    ),
                ])),
                element_nullable: false,
            }
        );
    }

    #[test]
    fn map_value_tuple_fields_keep_nullability() {
        let (column_type, _) = convert_clickhouse_type_to_column_type(
            "Map(String, Tuple(score Nullable(Float64)))",
        )
        .unwrap();

        match column_type {
            ColumnType::Map { value_type, .. } => assert_eq!(
                *value_type,
                ColumnType::NamedTuple(vec![(
                    "score".to_string(),
                    ColumnType::Nullable(Box::new(ColumnType::Float(FloatType::Float64)))
                )])
            ),
            other => panic!("expected Map, got {other:?}"),
        }
    }
```

If `FloatType` is not in scope in that module, add `use crate::framework::core::infrastructure::table::FloatType;` at its top.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test tuple_fields_keep_nullability 2>&1 | grep -E "test |panicked|left|right" | head`
Expected: both FAIL on `assert_eq!`, with `left` showing `ColumnType::Boolean` / `Float(Float64)` where `right` has the `Nullable` wrapper.

- [ ] **Step 3: Implement**

In `convert_ast_to_column_type`, in the `ClickHouseTypeNode::Tuple(elements)` arm, replace

```rust
                    TupleElement::Named { name, type_node } => {
                        let (field_type, _) = convert_ast_to_column_type(type_node)?;
                        fields.push((name.clone(), field_type));
                    }
```

with

```rust
                    TupleElement::Named { name, type_node } => {
                        // Tuple fields have no `required` flag like Nested
                        // columns do, so nullability lives in the type,
                        // as for JSON typed paths above.
                        let (field_type, nullable) = convert_ast_to_column_type(type_node)?;
                        let field_type =
                            if nullable && !matches!(field_type, ColumnType::Nullable(_)) {
                                ColumnType::Nullable(Box::new(field_type))
                            } else {
                                field_type
                            };
                        fields.push((name.clone(), field_type));
                    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test tuple_fields_keep_nullability 2>&1 | grep -E "test result|FAILED"`
Expected: both PASS. Then `cargo test 2>&1 | grep "test result"` — every suite `ok`.

- [ ] **Step 5: Confirm the harness moved**

Run: `TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1 --nocapture 2>&1 | tail -20`
Expected: still FAIL, listing only `view v_device_activity`.

- [ ] **Step 6: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add apps/cli/src/infrastructure/olap/clickhouse/type_parser.rs
git commit -F - <<'EOF'
fix: keep nullability on named tuple fields read back from ClickHouse

The type parser discarded the nullability of named tuple fields, so a
live Tuple(ok Nullable(Bool)) was reported as non-nullable. A column
declaring that field nullable never matched and was re-modified on
every plan, and a column declaring it non-nullable hid a real mismatch.

Nullable fields are now wrapped in ColumnType::Nullable, as JSON typed
paths already are.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 5: Compare views by their DDL only

**Files:**
- Modify: `apps/cli/src/framework/core/infra_reality_checker.rs:196-222` (`views_are_equivalent`), `:661-673` and `:755-762` (fallback logging)
- Modify: `apps/cli/src/framework/core/plan.rs:29` (import), `:97-140` (fallback logging)
- Test: `apps/cli/src/framework/core/infra_reality_checker.rs` `mod tests`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `views_are_equivalent(v1, v2, default_database)` keeps its signature and no longer reads `source_tables`.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `infra_reality_checker.rs`:

```rust
    #[test]
    fn test_views_ignore_declared_source_tables() {
        use crate::framework::core::infrastructure::view::View;

        let default_db = "mydb";
        // As declared in code: the user listed only the primary table.
        let declared = View {
            name: "v_device_activity".to_string(),
            database: None,
            select_sql: "SELECT r.id FROM readings AS r INNER JOIN devices AS d ON r.id = d.id"
                .to_string(),
            source_tables: vec!["`readings`".to_string()],
            metadata: None,
        };
        // As read back: the parser found both tables.
        let read_back = View {
            database: Some(default_db.to_string()),
            source_tables: vec!["readings".to_string(), "devices".to_string()],
            ..declared.clone()
        };
        assert!(
            views_are_equivalent(&declared, &read_back, default_db),
            "identical SQL must not be recreated because the declared dependencies differ"
        );

        let changed_sql = View {
            select_sql: "SELECT r.id FROM readings AS r".to_string(),
            ..read_back.clone()
        };
        assert!(!views_are_equivalent(&declared, &changed_sql, default_db));

        let moved = View {
            database: Some("otherdb".to_string()),
            ..read_back.clone()
        };
        assert!(!views_are_equivalent(&declared, &moved, default_db));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test test_views_ignore_declared_source_tables 2>&1 | grep -E "panicked|identical SQL"`
Expected: FAIL with `identical SQL must not be recreated because the declared dependencies differ`.

- [ ] **Step 3: Implement**

In `infra_reality_checker.rs`, replace `views_are_equivalent` and its doc comment with:

```rust
/// Checks if two Views are semantically equivalent.
///
/// A view's DDL is its name, its database and its SELECT, so those are all
/// that is compared; `select_sql` must already be normalized. `source_tables`
/// is deliberately ignored: on the code side it is the dependency list the
/// user declares, on the read-back side it is whatever the parser recovers
/// from the SQL, and the two routinely disagree for joins and subqueries
/// without the view differing at all. It still drives dependency ordering.
pub fn views_are_equivalent(v1: &View, v2: &View, default_database: &str) -> bool {
    v1.name == v2.name
        && normalize_database(&v1.database, default_database)
            == normalize_database(&v2.database, default_database)
        && v1.select_sql == v2.select_sql
}
```

- [ ] **Step 4: Make normalization fallbacks visible**

These fallbacks did not cause the churn, but each one silently guarantees a drop-and-recreate if normalization ever fails.

In `apps/cli/src/framework/core/plan.rs`, change line 29 to `use tracing::{debug, error, info, warn};`. In `normalize_infra_map_for_comparison`, in both the materialized-view loop and the view loop, change the `Err(e) => { debug!(...) }` arm's macro from `debug!` to `warn!` and its message to name the consequence:

```rust
            Err(e) => {
                warn!(
                    "Failed to normalize View '{}' SQL, comparing it as written; \
                     this can report a change on every plan: {:?}",
                    name, e
                );
            }
```

(use `MV '{}'` in the materialized-view loop).

In `infra_reality_checker.rs`, the MV block (`unwrap_or_else(|e| { debug!("Failed to normalize actual SQL for MV '{}': {:?}", id, e); ... })` and its `desired` twin): change both `debug!` to `warn!`. The view block uses `.unwrap_or_else(|_| actual.select_sql.clone())` and `.unwrap_or_else(|_| desired.select_sql.clone())`; replace each with:

```rust
                    .unwrap_or_else(|e| {
                        warn!("Failed to normalize actual SQL for view '{}': {:?}", id, e);
                        actual.select_sql.clone()
                    });
```

and

```rust
                    .unwrap_or_else(|e| {
                        warn!("Failed to normalize desired SQL for view '{}': {:?}", id, e);
                        desired.select_sql.clone()
                    });
```

`warn` is already imported in that file.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test views_ 2>&1 | grep -E "test result|FAILED"`
Expected: `test_views_ignore_declared_source_tables` and the existing `test_views_are_equivalent_pre_normalized` PASS. Then `cargo test 2>&1 | grep "test result"` — every suite `ok`.

- [ ] **Step 6: Confirm the harness is green**

Run: `TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1 2>&1 | grep "test result"`
Expected: `test result: ok. 1 passed`. This is the spec's primary goal — record the output for the final report.

- [ ] **Step 7: Lint and commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add apps/cli/src/framework/core/infra_reality_checker.rs apps/cli/src/framework/core/plan.rs
git commit -F - <<'EOF'
fix: compare views by their DDL, not their declared dependencies

View equivalence also compared source tables. The code side holds the
dependency list the user declares; the read-back side holds whatever
the parser recovers from the stored SQL. They disagree whenever a view
joins a table it does not declare, and sqlparser rejects joins whose
tables carry FINAL, so such views were dropped and recreated on every
plan with identical SQL.

Views now compare name, database and normalized SELECT. Source tables
still drive dependency ordering. SQL normalization fallbacks now log a
warning instead of a debug line.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

---

### Task 6: Enforce in CI and document

**Files:**
- Modify: `.github/workflows/test.yaml` (new job after `rust`)
- Modify: `AGENTS.md:55-61` (Testing Philosophy)
- Modify: `MIGRATION.md` (new section at the top)

**Interfaces:**
- Consumes: `docker-compose.test.yml` and `live_reality_roundtrip_plans_nothing` from Task 1; Tasks 2–5 must be merged so the job is green.
- Produces: CI job `reality-roundtrip`.

- [ ] **Step 1: Add the CI job**

In `.github/workflows/test.yaml`, after the `rust` job and before `typescript`, add:

```yaml
  reality-roundtrip:
    name: Reality round-trip (live ClickHouse)
    runs-on: ubuntu-latest
    steps:
      - name: Install Protoc
        uses: arduino/setup-protoc@v3
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}
          version: "23.x"

      - uses: actions/checkout@v4

      # No `toolchain:` input, for the same reason as the `rust` job.
      - uses: actions-rust-lang/setup-rust-toolchain@v1
        with:
          cache: true
          cache-shared-key: ${{ runner.os }}-${{ runner.arch }}-rust
          cache-on-failure: true
          cache-all-crates: true

      # The image tag lives only in docker-compose.test.yml, so local and CI
      # runs use the same server.
      - name: Start ClickHouse
        run: docker compose -f docker-compose.test.yml up -d --wait

      # Plans against a schema this test just created, so any reported change
      # is a phantom. Selects only this harness; mutations_live stays manual.
      - name: Plan against a freshly created schema
        env:
          TC_LIVE_CLICKHOUSE: "1"
        run: cargo test live_reality -- --test-threads=1
```

- [ ] **Step 2: Validate the workflow file**

Run: `python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/workflows/test.yaml')); print(list(d['jobs']))"`
Expected: `['rust', 'reality-roundtrip', 'typescript', 'library']`.

- [ ] **Step 3: Update AGENTS.md**

In `AGENTS.md`, replace

```markdown
- There are no E2E tests or templates in this repository. The downstream
  consumer pins published versions.
```

with

```markdown
- **Live-server tests** (`apps/cli/src/infrastructure/olap/clickhouse/*_live.rs`):
  run against a real ClickHouse and are skipped unless `TC_LIVE_CLICKHOUSE=1`.
  Start one with `docker compose -f docker-compose.test.yml up -d --wait`, then
  `TC_LIVE_CLICKHOUSE=1 cargo test live_ -- --test-threads=1`. CI runs
  `live_reality` (the planner round-trip) on every PR; `mutations_live` is
  manual.
- There are no templates in this repository. The downstream consumer pins
  published versions.
```

- [ ] **Step 4: Add the upgrade note**

In `MIGRATION.md`, insert directly after the opening paragraph block (before the first `---`), a new section:

```markdown
## Upgrading within 0.x: plans stop repeating no-op changes

Plans used to report changes that applying never removed: date-time string
columns, `Delta`/`Gorilla` codecs on 8-byte types, nullable named-tuple fields,
and views whose declared `baseTables` differ from their SQL. After upgrading,
those disappear, and a plan against an unchanged deployment is empty.

One change can appear once. The CLI now reads nullability inside named tuples
correctly. If a live column has a `Nullable` tuple field that your code declares
non-nullable, that mismatch was previously invisible and now shows as a
`MODIFY COLUMN`. It is real: either apply it, or declare the field nullable to
match the table.

Editing only a view's `baseTables` no longer recreates the view, since its
definition is unchanged. The new list still takes effect for dependency
ordering.
```

- [ ] **Step 5: Full verification**

Run each and confirm:

```bash
cargo fmt --check
cargo build
cargo clippy --all-targets -- -D warnings
cargo test 2>&1 | grep "test result"
pnpm --filter @typed-clickhouse/lib test 2>&1 | tail -3
TC_LIVE_CLICKHOUSE=1 cargo test live_reality -- --test-threads=1 2>&1 | grep "test result"
```

Expected: formatting clean, build and clippy clean, every Rust suite `ok`, TypeScript suite passing, live harness `1 passed`.

- [ ] **Step 6: Check the no-external-names rule**

Run: `git diff main | grep -E "^\+" | grep -oE "\b[a-z_]+_(readings|activity|window)\b|\bdevices\b|\bsensor_readings\b" | sort -u`
Then read `git diff main` once, end to end. Expected: every table, view and column name in the diff is one of the synthetic fixtures listed in Global Constraints, and no commit message, comment or doc names an external project or its schema. Reword anything that does before committing.

- [ ] **Step 7: Commit**

```bash
git add .github/workflows/test.yaml AGENTS.md MIGRATION.md
git commit -F - <<'EOF'
ci: run the planner round-trip against a live ClickHouse on every PR

Adds a job that starts ClickHouse from docker-compose.test.yml and runs
the round-trip harness, so a comparison that cannot survive a trip
through ClickHouse fails CI instead of producing a plan that never
converges.

Documents the live-server test modules in AGENTS.md and adds an upgrade
note to MIGRATION.md, including the one-time MODIFY COLUMN that correct
tuple nullability can surface.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
```

- [ ] **Step 8: Stop the local container**

Run: `docker compose -f docker-compose.test.yml down`
