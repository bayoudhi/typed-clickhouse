# Eliminating phantom diff operations

Date: 2026-09-23
Status: approved, not yet implemented

## Problem

The diff engine plans operations that change nothing and never converge. On a
deployment where the project code has not changed, two consecutive runs produce
byte-identical plans: the same columns are modified, the same views are dropped
and recreated, the same projection is dropped and re-added. Applying the plan
does not make the next plan smaller.

Every one of these operations originates in the same place: the code-side
representation of a schema object and the representation read back from
ClickHouse do not round-trip. The diff compares the two, finds a difference that
no DDL can ever remove, and emits an operation.

The cost is not only noise. Each `ALTER` waits on in-flight mutations, so a plan
full of no-op modifications stretches deployments by minutes. A projection
drop-and-add rebuilds projection data on a live table. Views are dropped and
recreated on every deploy, so anything querying them races a window where they
do not exist. And a plan that is never empty destroys the signal that a plan is
supposed to carry — nobody can tell a real migration from the standing noise.

### Observed distribution

From a plan of 136 operations, reproduced identically on two consecutive runs
against the same unchanged database:

| Count | Operation | Sole difference between the two sides |
|---|---|---|
| 86 | `ModifyTableColumn` | `annotations` |
| 22 | `ModifyTableColumn` | `codec` (`Delta` vs `Delta(8)`) |
| 2 | `ModifyTableColumn` | `data_type` (nullability inside a named tuple) |
| 12 + 12 | `DropView` + `CreateView` | view SELECT SQL |
| 1 + 1 | `DropTableProjection` + `AddTableProjection` | projection body |

## Root causes

Four distinct defects, three confirmed by reading the code, one carrying an open
probe.

### 1. Non-DDL annotations drive the column diff

`columns_are_equivalent` (`apps/cli/src/framework/core/infrastructure_map.rs:2441`)
and `nested_are_equivalent` (`apps/cli/src/infrastructure/olap/clickhouse/diff_strategy.rs:219`)
compare the full annotation vector for equality.

Most annotations are code-side metadata that ClickHouse cannot store. The
compiler plugin attaches `stringDate` to every `Format<"date-time">` field
(`packages/lib/src/dataModels/typeConvert.ts:507`) to tell the runtime
deserializer not to revive the value into a `Date`. It has no effect on
generated DDL: `std_field_type_to_clickhouse_type_mapper`
(`apps/cli/src/infrastructure/olap/clickhouse/mapper.rs:153-207`) reads exactly
three annotation keys — `simpleAggregationFunction`, `aggregationFunction`, and
`LowCardinality` — and ignores everything else.

The reality reader reconstructs annotations from the live database
(`apps/cli/src/infrastructure/olap/clickhouse/mod.rs:2687-2708`) for two cases
only: a `LowCardinality(` type prefix, and an extractable
`SimpleAggregateFunction`. `stringDate` is unrecoverable, so reality always
reports `annotations: []` while code reports `[["stringDate", true]]`.

Every `DateTime` column therefore differs forever.

### 2. Codec default parameters are resolved without the column type

`normalize_codec_expression` (`apps/cli/src/infrastructure/olap/clickhouse/mod.rs:3507`)
expands bare codec names to their defaults with a fixed table:

```rust
"Delta"    => "Delta(4)",
"Gorilla"  => "Gorilla(8)",
"ZSTD"     => "ZSTD(1)",
```

ClickHouse's actual default for `Delta` is `sizeof(type)`, not a constant. A
`UInt64` column declared `CODEC(Delta, ZSTD(1))` is stored as
`Delta(8), ZSTD(1)`, which never equals the normalized `Delta(4), ZSTD(1)`.
`Gorilla` has the same defect in the other direction.

### 3. Nullability is discarded inside named tuples

`convert_ast_to_column_type` (`apps/cli/src/infrastructure/olap/clickhouse/type_parser.rs:1712-1727`):

```rust
ClickHouseTypeNode::Tuple(elements) => {
    for element in elements.iter() {
        match element {
            TupleElement::Named { name, type_node } => {
                let (field_type, _) = convert_ast_to_column_type(type_node)?;
                fields.push((name.clone(), field_type));
            }
```

The nullability flag is bound to `_` and dropped. Reality's
`Array(Tuple(flag Nullable(Bool)))` parses back as a non-nullable field, so it
can never match a code-side nullable field.

The sibling branches handle this correctly: `Nested` (line 1671) preserves it as
`required: !is_nullable`, and the JSON typed-path branch (lines 1553-1559) wraps
the type in `ColumnType::Nullable`. The tuple branch is the outlier.

This is a read-back correctness bug independent of the churn. It causes the tool
to misreport the schema of a live table.

### 4a. Projection comparison reads the un-normalized map

`InfrastructureMap::diff_with_table_strategy`
(`apps/cli/src/framework/core/infrastructure_map.rs:1217`):

```rust
let projections_changed = table.projections != target_table.projections;
```

`table` and `target_table` are the original tables. `normalize_infra_map_for_comparison`
(`apps/cli/src/framework/core/plan.rs:142-156`) deliberately whitespace-collapses
every projection body for exactly this comparison, but writes the result into
the normalized map — which this line does not consult, while its immediate
neighbours (`partition_by`, table TTL) correctly use the normalized pair. The
normalization is computed and then discarded, so ClickHouse's multi-line stored
DDL never matches a single-line authored body.

`indexes_changed` on line 1214 has the same shape and must be checked in the
same pass.

### 4b. View SQL normalization fails silently (open probe)

Both sides of a view comparison are normalized through ClickHouse's
`formatQuerySingleLine` before `views_are_equivalent` runs — in the plan path
(`apps/cli/src/framework/core/plan.rs:97-140`) and in the reality checker
(`apps/cli/src/framework/core/infra_reality_checker.rs:751-787`). Every one of
those calls handles failure by falling back to the raw string:

```rust
.unwrap_or_else(|_| actual.select_sql.clone())
```

When normalization fails, both sides revert to raw text, and authored SQL is
compared against ClickHouse-formatted SQL. That never matches, so the view is
dropped and recreated on every deploy, with a single `debug!` line as the only
evidence.

Two candidate causes, not yet distinguished:

- `formatQuerySingleLine` rejecting unbound `{name:Type}` query-parameter
  placeholders, which parameterized views contain by definition.
- `source_tables` differing between the authored and read-back forms — the sets
  are compared independently of the SQL, and alias-heavy or subquery-heavy
  views extract differently on each side.

The reproduction harness settles this. Both candidates are in scope; the
silent-fallback behavior changes regardless of which one is responsible.

## Goals

- A plan run twice against an unchanged database produces an empty change set
  the second time.
- A committed, runnable reproduction proves the defect before the fix and the
  absence of it after.
- The bug class — code-side representation that cannot survive a round trip
  through ClickHouse — is structurally guarded, not just patched at today's four
  sites.

## Non-goals

- Persisting code-side metadata into ClickHouse. The
  `[MOOSE_METADATA:DO_NOT_MODIFY]` column-comment channel could carry
  `stringDate`, but that requires a one-time `ALTER` against every existing
  column of every existing deployment. The fix stays comparison-side. If a
  future annotation is genuinely DDL-relevant and needs round-tripping, that
  channel is the documented option.
- Rewriting the diff core to compare rendered DDL instead of the intermediate
  `Column` struct. That is a cleaner invariant and remains a reasonable future
  direction, but it has a large blast radius and would surface new mismatches
  before removing the old ones.

## Design

### Reproduction harness

A Rust integration test at `apps/cli/tests/reality_roundtrip.rs`, gated on the
`TCH_TEST_CLICKHOUSE_URL` environment variable. When the variable is absent the
test skips with a printed reason, so `cargo test` stays container-free for
contributors without Docker. A `docker-compose.test.yml` at the repository root
pins the ClickHouse image for local use.

The loop:

1. Build a fixture `InfrastructureMap` in Rust.
2. Render it to DDL through the production path (`std_column_to_clickhouse_column`
   into `CREATE TABLE` / `CREATE VIEW` / `ADD PROJECTION`) and execute it
   against the container.
3. Read reality back with the production reader — `ConfiguredDBClient`'s
   `OlapOperations` implementation — with no test double.
4. Run the production comparison: `normalize_infra_map_for_comparison` on both
   maps, then `diff_with_table_strategy`.
5. Assert the change set is empty.

The harness is self-proving: step 2 creates the database objects from the same
fixture step 4 compares against, so any operation produced is a phantom by
construction. No judgment call is needed about whether a given diff is real.

Failure output names the responsible carrier —
`table.column: <field> differs (reality=… code=…)` — so a regression reports
which fixture broke rather than that the diff was non-empty.

### Fixture schema

Synthetic, with one carrier per defect class:

| Fixture | Carries |
|---|---|
| `sensor_readings.recordedAt`, `.ingestedAt` — `DateTime(3)` with the `stringDate` annotation | class 1 |
| `sensor_readings._version` — `UInt64 CODEC(Delta, ZSTD(1))` | class 2 |
| `sensor_readings.samples` — `Array(Nested(...))` with a nullable member, which ClickHouse reports back as `Array(Tuple(...))` and so exercises the tuple branch | class 3 |
| `sensor_readings.p_by_device` — projection with an `ORDER BY` | class 4a |
| `v_daily_totals` — plain view; `v_readings_in_window` — parameterized view using `{from:String}` | class 4b |

The fixture is constructed in Rust rather than compiled from TypeScript, so it
cannot drift silently from what the compiler plugin emits. A companion assertion
in `packages/lib` tests that a `Format<"date-time">` field still produces the
`stringDate` annotation; if the plugin's output changes, that test fails and the
fixture is updated deliberately.

### Fix 1 — annotation relevance registry

Add to the ClickHouse layer:

```rust
/// Annotations that change generated DDL. Everything else is code-side
/// metadata ClickHouse cannot store, so it must not drive a diff.
const DDL_RELEVANT_ANNOTATIONS: [&str; 3] =
    ["LowCardinality", "aggregationFunction", "simpleAggregationFunction"];
```

`columns_are_equivalent` and `nested_are_equivalent` filter both annotation
vectors through it before comparing.

The list is derived from what `mapper.rs:153-207` actually reads when building a
ClickHouse type, not chosen by inspection of current usage. The default inverts:
an annotation is ignored unless registered as DDL-affecting. Adding a
DDL-affecting annotation later without registering it here fails the round-trip
test.

### Fix 2 — type-aware codec defaults

Thread the column's `ColumnType` through `codec_expressions_are_equivalent` and
`normalize_codec_expression`. Resolve `Delta` and `Gorilla` default widths from
the type:

| Width | Types |
|---|---|
| 8 | `Int64`, `UInt64`, `Float64`, `DateTime64`, `Decimal64` |
| 4 | `Int32`, `UInt32`, `Float32`, `DateTime`, `Date32` |
| 2 | `Int16`, `UInt16`, `Date` |
| 1 | `Int8`, `UInt8`, `Bool` |

For any type whose width is not known, leave the codec unexpanded rather than
substituting a guess.

### Fix 3 — preserve tuple field nullability

In the `ClickHouseTypeNode::Tuple` branch of `convert_ast_to_column_type`, keep
the nullability flag and wrap the field type, following the pattern the JSON
typed-path branch already uses:

```rust
let (field_type, nullable) = convert_ast_to_column_type(type_node)?;
let field_type = if nullable && !matches!(field_type, ColumnType::Nullable(_)) {
    ColumnType::Nullable(Box::new(field_type))
} else {
    field_type
};
```

### Fix 4a — compare normalized projections and indexes

In `diff_with_table_strategy`, read `normalized_table` / `normalized_target` for
the projection and index comparisons, matching how `partition_by` and table TTL
are already handled.

### Fix 4b — resolve and expose view normalization failure

Ordered by the probe, which runs first:

1. Determine why `formatQuerySingleLine` fails or produces divergent output for
   the harness's parameterized view, and fix that specific cause — either
   normalizing around parameter placeholders, or correcting `source_tables`
   extraction so the two sides agree.
2. Independently of the cause, change the fallback: normalization failure logs
   at `warn!` with the view name and the underlying error. A silent fallback
   that guarantees a drop-and-recreate must never again be invisible.

## Testing

Fast tier, no container, inside the existing `cargo test`:

| Test | Asserts |
|---|---|
| annotation filter | columns differing only by `stringDate` are equivalent; differing by `LowCardinality` are not |
| codec width | `Delta` ≡ `Delta(8)` for `UInt64`; `Delta` ≡ `Delta(4)` for `UInt32`; unknown-width type leaves `Delta` unexpanded and unequal to `Delta(8)` |
| tuple nullability | `Tuple(a Nullable(Bool))` round-trips to a field equal to the code-side nullable field |
| projection comparison | a table differing only in projection-body whitespace yields no change |
| TS guard (`packages/lib`) | a `Format<"date-time">` field still emits the `stringDate` annotation |

Container tier, env-gated: the create → read back → diff loop, asserting an
empty change set and naming the carrier on failure.

### CI

A new `reality-roundtrip` job in `.github/workflows/test.yaml` using a GitHub
service container. The image tag lives in `docker-compose.test.yml` and the
workflow reads it from there, so the local and CI versions cannot drift. The job
sets `TCH_TEST_CLICKHOUSE_URL` and runs `cargo test --test reality_roundtrip`.

The existing `rust` job is unchanged, so contributors without Docker see no
difference. `AGENTS.md` currently states the repository has no end-to-end tests;
that line is amended rather than left inaccurate.

## Risks

**Failure direction is deliberate.** A wrong codec-width entry, or a type whose
width is unknown, leaves the codec unexpanded — worst case a phantom operation
survives, which is today's behavior. No fix here can cause a real change to go
missing, with one exception.

**The exception is the annotation filter.** Filtering annotations out of the
comparison could in principle mask a real change. Two mitigations: the registry
is derived from the renderer's actual reads rather than from inspection, and the
round-trip test fails if a DDL-affecting annotation is ever added without being
registered.

**Fix 3 changes what the reader reports for existing deployments.** Where a live
table genuinely has non-nullable tuple fields while code declares them nullable,
that mismatch is currently invisible; after the fix it becomes a real and
correct `MODIFY COLUMN`. Users will see a one-time `ALTER` that looks new but is
the tool finally reporting the truth. This needs a `MIGRATION.md` note, and it
means the change ships as a minor version rather than a patch.

## Sequencing

1. Harness, fixtures, and CI job — landing a failing reproduction before any
   fix.
2. Probe for class 4b, the only fix whose shape is still open.
3. The four fixes, each with its fast-tier test, each independently revertable.
4. `MIGRATION.md` note and the `AGENTS.md` amendment.
