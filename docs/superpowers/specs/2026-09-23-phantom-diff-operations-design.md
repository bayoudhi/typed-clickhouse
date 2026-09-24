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
| 12 + 12 | `DropView` + `CreateView` | view `source_tables` |
| 1 + 1 | `DropTableProjection` + `AddTableProjection` | none — cascade from the column modifications above |

## Root causes

Four distinct defects, each confirmed by reading the code and probing a real
ClickHouse 25.8 server. The projection churn in the table above is not a fifth
defect; see "Projection churn is a cascade" below.

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

ClickHouse's actual default for `Delta` is `sizeof(type)`, not a constant.
Probed on 25.8: a `UInt64` column declared `CODEC(Delta, ZSTD(1))` is stored as
`Delta(8), ZSTD(1)`, and so is a `DateTime(3)` column, which ClickHouse stores as
the 8-byte `DateTime64(3)`. Neither ever equals the normalized
`Delta(4), ZSTD(1)`. `Gorilla` has the same defect in the other direction.

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

### 4. View comparison checks a user-declared list against a parsed one

`views_are_equivalent` (`apps/cli/src/framework/core/infra_reality_checker.rs:199-222`)
compares two things: the normalized `select_sql`, and `source_tables` as a set.
The two sides of `source_tables` are different kinds of information:

- Code side: the `baseTables` array the user passes to the TypeScript `View`
  constructor (`packages/lib/src/dmv2/sdk/view.ts:57`). Its documented purpose
  is dependency tracking. Nothing checks it against the SQL.
- Reality side: whatever `extract_source_tables_from_query` can recover from the
  stored SELECT (`apps/cli/src/infrastructure/olap/clickhouse/mod.rs:3323-3337`),
  falling back to a regex extractor when sqlparser rejects the query.

Probing the extractor showed where the two disagree. sqlparser rejects a JOIN
whose tables carry `FINAL` (`Expected: end of statement, found: FINAL`), so those
views go through the regex fallback; and on a single-table `FINAL` query it
parses `FINAL` as a table alias. Any view whose declared `baseTables` does not
exactly equal the extractor's output — a joined table left undeclared, a view
listed as a base, a subquery — is reported as changed on every run.

The SQL comparison itself is sound. The probe ran `normalize_sql_for_comparison`
over plain, parameterized (`{from:String}`), `FINAL`, JOIN-with-`FINAL`, and
subquery views; in every case the authored and read-back forms normalized to
identical text. `formatQuerySingleLine` also accepts query-parameter
placeholders. An earlier hypothesis that parameterized views break
normalization is therefore ruled out.

A view's DDL is its SELECT and nothing else. `source_tables` drives dependency
ordering; it has no representation in ClickHouse, so it must not decide whether
a view is recreated.

### Projection churn is a cascade

`drop_column_dependents` (`apps/cli/src/infrastructure/olap/ddl_ordering.rs:1148-1175`)
drops every projection that references a column being modified, and
`readd_column_dependents` re-adds it afterwards. That is correct behavior for a
real column change. Here the projection referenced columns that were being
phantom-modified by defects 1 and 2, so it was torn down and rebuilt as a side
effect.

Probing confirmed the projection itself round-trips: an authored body and the
body ClickHouse stores are identical after the existing whitespace collapse in
`normalize_infra_map_for_comparison`, including with keyword-like identifiers.
No projection-specific fix is needed. The harness still asserts that no
projection operation appears, so the cascade is covered.

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

An inline `#[cfg(test)]` module at
`apps/cli/src/infrastructure/olap/clickhouse/reality_roundtrip_live.rs`. It has to
be inline: the CLI crate is binary-only, so a test under `apps/cli/tests/`
cannot reach its internals.

It follows the convention `mutations_live.rs` already established for
live-server tests: skipped unless `TC_LIVE_CLICKHOUSE=1` is set, connection
details overridable through `TC_LIVE_CH_HOST`, `TC_LIVE_CH_PORT`,
`TC_LIVE_CH_USER` and `TC_LIVE_CH_PASSWORD`, test names prefixed `live_`. So
`cargo test` stays hermetic for contributors without Docker. A
`docker-compose.test.yml` at the repository root pins the ClickHouse image and
matches those defaults.

The loop:

1. Build a fixture `InfrastructureMap` in Rust.
2. Apply it through the production path: diff an empty map against the
   fixture, then hand the resulting changes to `olap::execute_changes` — the
   same executor `migrate` uses.
3. Reconcile with reality exactly as `plan_changes` does, through
   `reconcile_with_reality` and the real `ConfiguredDBClient`, with no test
   double.
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
| `sensor_readings.recordedAt`, `.ingestedAt` — `DateTime(3)` with the `stringDate` annotation | defect 1 |
| `sensor_readings._version` — `UInt64 CODEC(Delta, ZSTD(1))`; `recordedAt` also carries `CODEC(Delta, ZSTD(1))` | defect 2 |
| `sensor_readings.samples` — `Array(Tuple(label String, ok Nullable(Bool)))` | defect 3 |
| `v_device_activity` — JOIN of `sensor_readings` and `devices`, both with `FINAL`, declaring only `sensor_readings` as a source table | defect 4 |
| `v_readings_in_window` — parameterized view using `{from:String}`, declaring its one source table correctly | control: must stay unchanged before and after |
| `sensor_readings.p_by_device` — projection over `deviceId`, `recordedAt`, `_version` | the projection cascade |

The defect-3 carrier is an explicit `Array(Tuple(...))`, not `Nested`: the
production client sets `flatten_nested = 0`, so a `Nested` column reads back as
`Nested` and never reaches the tuple branch.

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
| 8 | `Int64`, `UInt64`, `Float64`, `DateTime` with a precision (stored as `DateTime64`) |
| 4 | `Int32`, `UInt32`, `Float32`, `DateTime` without a precision, `Date32` |
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

### Fix 4 — views compare their DDL only

Remove the `source_tables` comparison from `views_are_equivalent`. The view's
name, its database, and its normalized `select_sql` fully determine the DDL.
`source_tables` keeps its job — dependency ordering in `ddl_ordering` — but no
longer decides whether a view is recreated.

Materialized views keep their `source_tables` comparison. None churned, and an
MV's target and source wiring is part of what makes it behave, so changing it is
out of scope here.

Separately, the normalization fallbacks in `normalize_infra_map_for_comparison`
(`apps/cli/src/framework/core/plan.rs:97-140`) and in the reality checker log at
`debug!`. They are raised to `warn!` with the object name and the error. The
fallback did not cause this churn, but a silent fallback that would guarantee a
drop-and-recreate should not be invisible if it ever does.

## Testing

Fast tier, no container, inside the existing `cargo test`:

| Test | Asserts |
|---|---|
| annotation filter | columns differing only by `stringDate` are equivalent; differing by `LowCardinality` are not |
| codec width | `Delta` ≡ `Delta(8)` for `UInt64`; `Delta` ≡ `Delta(4)` for `UInt32`; unknown-width type leaves `Delta` unexpanded and unequal to `Delta(8)` |
| tuple nullability | `Tuple(a Nullable(Bool))` round-trips to a field equal to the code-side nullable field |
| view equivalence | two views with identical SQL and different `source_tables` are equivalent; different SQL is not |
| TS guard (`packages/lib`) | a `Format<"date-time">` field still emits the `stringDate` annotation |

Container tier, env-gated: the create → read back → diff loop, asserting an
empty change set and naming the carrier on failure.

### CI

A new `reality-roundtrip` job in `.github/workflows/test.yaml` that starts
ClickHouse with `docker compose -f docker-compose.test.yml up -d --wait`, so the
image tag lives in one file and local and CI runs cannot drift. The job sets
`TC_LIVE_CLICKHOUSE=1` and runs `cargo test live_reality -- --test-threads=1`,
which selects only this harness; the heavier `mutations_live` tests stay
manual.

The existing `rust` job is unchanged, so contributors without Docker see no
difference. `AGENTS.md` states the repository has no end-to-end tests, which was
already inaccurate once `mutations_live.rs` landed; the line is amended to
describe the live-server test modules and how to run them.

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

**Fix 3 changes what the reader reports for existing deployments.** Today the
reader reports every tuple field as non-nullable. So where a live table has a
`Nullable` tuple field but code declares it non-nullable, the two currently look
equal and the mismatch is invisible. After the fix it becomes a real and correct
`MODIFY COLUMN`. Users will see a one-time `ALTER` that looks new but is the tool
finally reporting the truth. This needs a `MIGRATION.md` note, and it means the
change ships as a minor version rather than a patch.

**Fix 4 stops recreating views whose declared dependencies change.** If a user
edits only a view's `baseTables`, no DDL is emitted. That is correct, since the
view's definition is unchanged, and the new list still reaches the stored state
and dependency ordering.

## Sequencing

1. Harness, fixtures, and CI job — landing a failing reproduction before any
   fix.
2. The four fixes, each with its fast-tier test, each independently revertable.
3. `MIGRATION.md` note and the `AGENTS.md` amendment.
