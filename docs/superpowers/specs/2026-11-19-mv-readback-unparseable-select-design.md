# Read back materialized views whose SELECT the parser cannot handle

Date: 2026-11-19
Status: draft (short design, small change)

## Problem

On 0.3.1, a structured `MaterializedView` whose SELECT the forked `sqlparser`
cannot parse is never recognised once it is in the database. Examples are tuple
element access `x.2`, which ClickHouse also writes back for `tupleElement(x, 2)`,
and `LEFT JOIN db.t AS p FINAL`. The steps are:

1. `reconstruct_sql_resource_from_mv` rebuilds the view's setup text from
   `system.tables`. Source tables come from a regex fallback, which logs
   `Could not parse … using regex fallback`.
2. `try_migrate_sql_resource_to_mv` then calls `parse_create_materialized_view`
   (sqlparser). That fails, `.ok()?` returns `None`, and the view stays an
   unstructured `SqlResource` in reality.
3. The code side holds a structured view with that name, which reality
   lacks. Every plan therefore emits `CreateMaterializedView`, and it runs as
   `CREATE MATERIALIZED VIEW IF NOT EXISTS`, which does nothing.

Consequences, measured on ClickHouse 25.8:
- **A plan is never empty.**
- **A SELECT change never reaches the database.** The view silently keeps its
  old definition.
- **Removing the view from code never drops it.**

This happens for incremental and refreshable views alike. Plain views do not
have the problem, because their comparison does not depend on sqlparser
succeeding.

The comparison step itself is fine. `normalize_infra_map_for_comparison` and
the reality checker normalise MV SQL with ClickHouse's own
`formatQuerySingleLine`, which handles these queries.

## Goal

A materialized view whose SELECT sqlparser rejects round-trips like any other:
- an unchanged plan is empty;
- a SELECT change is a drop + create of the view only;
- a schedule-only change on a refreshable view is `MODIFY REFRESH`;
- removing the view from code drops it.

## Non-goals

- Making the forked sqlparser understand more ClickHouse syntax.
- Changing how source tables are extracted. The existing regex fallback stays.

## Approach (one sensible way)

When `parse_create_materialized_view` fails, `try_migrate_sql_resource_to_mv`
falls back to a text split of the same statement instead of returning `None`:

- **Header:** everything before the first top-level `AS` that follows the `TO`
  target.
  - The `REFRESH` clause comes from `split_refresh_clause`, already used here.
  - The target and target database come from the header, the same way
    `reconstruct_sql_resource_from_mv` finds them.
- **SELECT:** the text after that `AS`, unchanged. Normalisation happens later,
  through ClickHouse.
- **Source tables:** `extract_source_tables_from_query_regex`, the same
  fallback reconstruct already uses. For refreshable views they are not
  compared anyway.
- **Logging:** the conversion logs at `debug!`. The plan stays quiet about a
  parse failure that it now handles.

The fallback keeps every existing guard of `try_migrate_sql_resource_to_mv`:
- exactly one setup and one teardown statement;
- the `CREATE MATERIALIZED VIEW IF NOT EXISTS` / `DROP VIEW IF EXISTS` shapes;
- a ` TO ` present.

It only replaces the sqlparser step.

Why not the alternatives:
- Teaching the forked sqlparser ClickHouse tuple access and aliased `FINAL` is
  open-ended. The next unsupported construct would bring the same bug back.
- Failing loudly on unparseable views would be honest, but it blocks
  legitimate ClickHouse SQL that the server already accepts.

## Noted, no change: timezone-less `DateTime`

The DDL writes a timezone-less `DateTime` column as `DateTime('UTC')`, and the
type parser reads both forms as the same column type, so the tool itself never
shows a phantom diff. No library change is needed for that. A consumer that
compares `system.columns.type` strings directly has to treat the two as equal.

## Tests

- **Unit:**
  - `try_migrate_sql_resource_to_mv` on a setup whose SELECT contains `x.2` and
    `LEFT JOIN db.t AS p FINAL` returns a structured view with the right name,
    target, select text and refresh schedule.
  - The same with no refresh clause, to show incremental views also convert.
  - Existing tests are unchanged.
- **Live (`reality_roundtrip_live.rs`):** add a refreshable view to the fixture
  whose SELECT uses tuple element access and an aliased `FINAL` join.
  - `live_reality_roundtrip_plans_nothing` stays empty.
  - A SELECT change is exactly one drop + one create of that view.
  - Removing it from the code-side map plans its drop.

## Release

`v0.3.2` once merged, through the existing tag workflow. The darwin-arm64 build
now runs on `macos-15`.
