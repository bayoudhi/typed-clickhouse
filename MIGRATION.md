# Migrating to typed-clickhouse 0.1.0

From `@bayoudhi/moose-lib-serverless@0.7.13` + `@bayoudhi/moose-cli@1.0.0`
to `@typed-clickhouse/core@0.1.0` + `@typed-clickhouse/cli@0.1.0`.

Nothing about your ClickHouse tables changes. What changes is every name the
tool uses to refer to itself: the packages, the binary, the config file, the
environment-variable prefix, and the project-local working directory.

**Read [Before you start](#before-you-start) first.** If your project uses any
capability listed there, it cannot migrate to 0.1.0 at all.

---

## Before you start

`typed-clickhouse` is a ClickHouse schema-migration tool. It is a deliberately
narrowed fork, and these capabilities were removed rather than renamed:

| Removed | What it was |
| --- | --- |
| Streaming | `Stream`, `DeadLetterQueue`, Kafka/Redpanda transport, streaming functions |
| Workflows | `Task`, `Workflow`, the Temporal integration |
| Ingest APIs | `IngestApi`, `IngestPipeline`, the HTTP ingest server |
| The dev server | `moose dev`, `moose prod`, the local Docker infrastructure it managed |
| Scaffolding | `moose init`, `moose template` |

A project that imports `Stream`, `IngestPipeline`, `Task` or `Workflow`, or that
runs `moose dev` as its development loop, has no migration path here. Stay on
the version you are on.

What survives is the ClickHouse half: `OlapTable`, `View`, `MaterializedView`,
`SelectRowPolicy`, `SqlResource`, the typed query layer (`QueryClient`,
`defineQueryModel`, `createQueryHandler`), `WebApp`, and the
plan/migrate/`db pull`/seed commands that operate on them.

---

## 1. Dependencies

```diff
 {
   "dependencies": {
-    "@bayoudhi/moose-lib-serverless": "0.7.13"
+    "@typed-clickhouse/core": "0.1.0"
   },
   "devDependencies": {
-    "@bayoudhi/moose-cli": "1.0.0"
+    "@typed-clickhouse/cli": "0.1.0"
   }
 }
```

The version number going *backwards* (0.7.13 → 0.1.0) is intentional: this is a
new package line starting from 0.1.0, not a downgrade. Nothing keys behaviour off
the release version — the state-compatibility guard uses the infrastructure map's
own `data_model_version`, so 0.1.0 is safe to ship and safe to adopt.

Then reinstall so the new binaries land in `node_modules/.bin`:

```bash
rm -rf node_modules && npm install     # or pnpm install / yarn install
```

## 2. Imports

The specifier changes. The import surface does not.

```diff
-import { OlapTable, MaterializedView, QueryClient } from "@bayoudhi/moose-lib-serverless";
+import { OlapTable, MaterializedView, QueryClient } from "@typed-clickhouse/core";
```

Every symbol you imported before is still exported under the same name — including
the six that still read `MOOSE`:

```ts
import {
  MOOSE_RLS_ROLE,
  MOOSE_RLS_USER,
  MOOSE_RLS_SETTING_PREFIX,
  MOOSE_RUNTIME_ENV_PREFIX,
  mooseRuntimeEnv,
  mooseEnvSecrets,          // deprecated alias of mooseRuntimeEnv
} from "@typed-clickhouse/core";
```

They keep their names on purpose. `MOOSE_RLS_ROLE` and friends *are* the names of
ClickHouse objects in your live database (see §7), and renaming the constants
without renaming the objects would be worse than leaving both. Your code needs no
edit beyond the specifier.

A find-and-replace over your source is enough:

```bash
grep -rl '@bayoudhi/moose-lib-serverless' src app \
  | xargs sed -i '' 's|@bayoudhi/moose-lib-serverless|@typed-clickhouse/core|g'
```

## 3. The CLI

```diff
-npx moose plan --clickhouse-url "$URL"
+npx typed-clickhouse plan --clickhouse-url "$URL"
```

The binary is `typed-clickhouse`. The subcommands, flags and output are unchanged
for everything that survived the narrowing.

If your `package.json` had a `moose` script alias, point it at the new binary:

```diff
   "scripts": {
-    "moose": "moose",
-    "build": "moose build"
+    "tch": "typed-clickhouse",
+    "build": "typed-clickhouse build"
   }
```

The two helper binaries the CLI spawns were renamed too, from `moose-tspc` and
`moose-runner` to `tch-tspc` and `tch-runner`. You only need to know this if you
invoked them directly; the CLI resolves them itself.

## 4. Config file

```bash
git mv moose.config.toml tch.config.toml
```

**Optional but recommended.** `moose.config.toml` is still read as a fallback, so
an unmigrated project keeps working. Only the name the CLI *writes* changed. The
file's contents are unchanged.

If you keep the old name, expect it to stop being read in a future major version.

## 5. Environment variables

The prefix moved from `MOOSE_` to `TCH_`. **The old names are not read at all** —
there is no fallback here, unlike the config file. A `MOOSE_`-prefixed variable
is silently ignored, which looks like "my config is being ignored" rather than an
error, so this is the change most likely to bite.

The ones a real project sets:

| Before | After |
| --- | --- |
| `MOOSE_CLICKHOUSE_CONFIG__URL` | `TCH_CLICKHOUSE_CONFIG__URL` |
| `MOOSE_CLICKHOUSE_CONFIG__HOST` | `TCH_CLICKHOUSE_CONFIG__HOST` |
| `MOOSE_CLICKHOUSE_CONFIG__HOST_PORT` | `TCH_CLICKHOUSE_CONFIG__HOST_PORT` |
| `MOOSE_CLICKHOUSE_CONFIG__USER` | `TCH_CLICKHOUSE_CONFIG__USER` |
| `MOOSE_CLICKHOUSE_CONFIG__PASSWORD` | `TCH_CLICKHOUSE_CONFIG__PASSWORD` |
| `MOOSE_CLICKHOUSE_CONFIG__DB_NAME` | `TCH_CLICKHOUSE_CONFIG__DB_NAME` |
| `MOOSE_CLICKHOUSE_CONFIG__USE_SSL` | `TCH_CLICKHOUSE_CONFIG__USE_SSL` |
| `MOOSE_CLICKHOUSE_CONFIG__RLS_USER` | `TCH_CLICKHOUSE_CONFIG__RLS_USER` |
| `MOOSE_CLICKHOUSE_CONFIG__RLS_PASSWORD` | `TCH_CLICKHOUSE_CONFIG__RLS_PASSWORD` |
| `MOOSE_LOGGER__LEVEL` | `TCH_LOGGER__LEVEL` |
| `MOOSE_LOGGER__STDOUT` | `TCH_LOGGER__STDOUT` |
| `MOOSE_LOGGER__OTLP_ENDPOINT` | `TCH_LOGGER__OTLP_ENDPOINT` |

`RLS_USER` and `RLS_PASSWORD` are the credentials the query layer uses when a
`SelectRowPolicy` is in play — the pair a multi-tenant project must set. Miss
them and row-policy-protected queries fail to authenticate.

The rule is mechanical for anything not listed: `MOOSE_<REST>` becomes
`TCH_<REST>`, double underscores and all.

```bash
sed -i '' 's/^MOOSE_/TCH_/' .env .env.dev .env.prod .env.local 2>/dev/null
```

Check your CI secrets and deployment manifests too — those are the copies a `sed`
over the repository misses.

A handful of one-off variables moved as well: `MOOSE_SOURCE_DIR` →
`TCH_SOURCE_DIR`, `MOOSE_CLIENT_ONLY` → `TCH_CLIENT_ONLY`,
`MOOSE_DISABLE_COMPILER_LOGS` → `TCH_DISABLE_COMPILER_LOGS`.

## 6. tsconfig.json

The compiler plugin's transform path follows the package rename:

```diff
 {
   "compilerOptions": {
     "plugins": [
-      { "transform": "@bayoudhi/moose-lib-serverless/dist/compilerPlugin.js" },
+      { "transform": "@typed-clickhouse/core/dist/compilerPlugin.js" },
       { "transform": "typia/lib/transform" }
     ]
   }
 }
```

Getting this wrong is quiet, not loud: without the transform, your table schemas
are emitted empty and the CLI plans a migration that drops all your columns.
After editing, run `npx typed-clickhouse plan` and confirm the plan is empty
before you run anything that writes.

Two directories also moved, both of which you should gitignore:

```diff
-.moose/
+.tch/
```

Compiled output now lands in `.tch/compiled` instead of `.moose/compiled` (unless
your `tsconfig.json` sets an explicit `outDir`, which is still honoured), and the
CLI's own per-project scratch directory is `.tch/`. Delete the stale `.moose/`
directory; nothing reads it.

## 7. Your user-level CLI directory

This is separate from the project-local `.moose/` → `.tch/` rename in the
previous section. The CLI also keeps a directory in your **home** directory,
which moved the same way:

```diff
-~/.moose/
+~/.tch/
```

It holds two things, and unlike `tch.config.toml`, **neither has a fallback to
the old location** — the CLI only ever looks in `~/.tch/`:

- `~/.tch/config.toml` — global CLI settings (currently just logger config).
  If you had customized `~/.moose/config.toml`, the new CLI won't read it and
  silently falls back to defaults.
- `~/.tch/machine_id` — an anonymous tracking ID generated on first run. If
  `~/.tch/machine_id` doesn't exist, the CLI silently generates a new one the
  next time it runs, so anything keyed on the old ID (e.g. usage analytics
  continuity) resets.

Neither loss is destructive — both files are regenerated with sensible
defaults — but if you want continuity, copy them across yourself:

```bash
mkdir -p ~/.tch
cp ~/.moose/config.toml ~/.tch/config.toml 2>/dev/null
cp ~/.moose/machine_id ~/.tch/machine_id 2>/dev/null
```

The stale `~/.moose/` directory is not deleted automatically; remove it once
you've copied anything you want to keep.

## 8. Stored state: the first run will refuse

**This is the one step that requires a deliberate action against your database.**

The CLI records the infrastructure map it last applied in a ClickHouse table so
it can diff against it. Maps written by the old CLI carry no `data_model_version`
field, and the compatibility guard refuses to diff against a map whose data model
it cannot identify. Your first command fails with a refusal, not a plan.

This is the guard working. The alternative — diffing against a map it cannot
fully interpret — would produce a plan that drops resources it failed to parse.

The remedy is to discard the stored map:

```sql
DELETE FROM _MOOSE_STATE WHERE key LIKE 'infra_map_%';
```

Run it against the same ClickHouse database the CLI connects to. Then:

```bash
npx typed-clickhouse plan --clickhouse-url "$TCH_CLICKHOUSE_CONFIG__URL"
```

**No tables are dropped by this.** The map is a record of what the tool believes
it created, not the data itself. With no stored map, the CLI reads the live
database directly and re-adopts what it finds: existing tables, views and
materialized views are matched against your code by name and left in place. The
plan you get should be empty, or contain only changes you actually made to your
models. Read it before applying it — if it proposes dropping something, stop and
work out why rather than confirming.

`_MOOSE_STATE` keeps its name. Renaming the table would orphan the state of every
existing project, which is a far worse outcome than a table with a legacy name.
The same reasoning applies to a few other things baked into your live database,
which are **unchanged** and require no action from you:

- the `moose_rls_role` role and `moose_rls_user` user, created by row-policy DDL
- the `SQL_moose_rls_<column>` custom setting names referenced by row policies
- the `[MOOSE_METADATA:DO_NOT_MODIFY]` prefix on column comments, which carries
  enum and type metadata the CLI parses back out

## 9. Keyring credentials

If you let the old CLI save a ClickHouse URL to your OS keychain, it is stored
under the service name `moose-cli_{project}`. The new CLI looks under
`typed-clickhouse_{project}` and will not find it, so it behaves as if you had
never saved one.

Either re-enter it when prompted:

```bash
npx typed-clickhouse db pull       # prompts and re-saves under the new name
```

or bypass the keychain entirely:

```bash
npx typed-clickhouse plan --clickhouse-url "clickhouse://user:pass@host:8443/db"
```

The old entry is left in your keychain. Delete it by hand if you want it gone
(macOS: Keychain Access, search `moose-cli`).

---

## Checklist

```
[ ] No Stream / IngestPipeline / Task / Workflow imports, no `moose dev` loop
[ ] package.json dependencies swapped, node_modules reinstalled
[ ] import specifiers updated
[ ] tsconfig.json plugin transform path updated
[ ] moose.config.toml renamed to tch.config.toml (optional)
[ ] MOOSE_* environment variables renamed to TCH_*, including CI and deploys
[ ] .gitignore: .moose/ -> .tch/, stale .moose/ deleted
[ ] ~/.moose/ config.toml and machine_id copied to ~/.tch/ if you want continuity
[ ] DELETE FROM _MOOSE_STATE WHERE key LIKE 'infra_map_%'
[ ] ClickHouse URL re-entered, or passed with --clickhouse-url
[ ] `typed-clickhouse plan` reviewed and empty before anything is applied
```
