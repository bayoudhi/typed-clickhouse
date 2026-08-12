/**
 * Re-exports the the library compiler plugin from the workspace library.
 *
 * Previously this file was a placeholder that tsup replaced with a patched
 * copy of upstream's compiled plugin. The patches now live in
 * `packages/lib/src/compilerPluginHelper.ts`, so a plain re-export
 * is sufficient.
 */
export { default } from "@typed-clickhouse/lib/dist/compilerPlugin";
