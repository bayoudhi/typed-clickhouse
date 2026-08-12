/**
 * @fileoverview Row-level security (RLS) constants and helpers.
 *
 * Deliberately native-free: no Temporal, Redis, or Kafka imports. This lets
 * `serverless.ts` re-export these symbols without dragging streaming/workflow
 * native dependencies back into the serverless bundle. Keep it that way --
 * import only pure-TS modules (or type-only imports) here.
 */
import type { RowPolicyOptions } from "./consumption-apis/query-client";

/**
 * Shared ClickHouse role name used by all row policies.
 * IMPORTANT: Must match MOOSE_RLS_ROLE in apps/cli/src/framework/core/infrastructure/select_row_policy.rs
 */
export const MOOSE_RLS_ROLE = "moose_rls_role";

/**
 * Dedicated ClickHouse user for RLS queries.
 * Created at DDL time with SELECT-only permissions and the RLS role granted.
 * IMPORTANT: Must match MOOSE_RLS_USER in apps/cli/src/framework/core/infrastructure/select_row_policy.rs
 */
export const MOOSE_RLS_USER = "moose_rls_user";

/**
 * Prefix for ClickHouse custom settings used by row policies.
 * Setting names are `{MOOSE_RLS_SETTING_PREFIX}{column}`.
 * IMPORTANT: Must match the format in setting_name() in apps/cli/src/framework/core/infrastructure/select_row_policy.rs
 */
export const MOOSE_RLS_SETTING_PREFIX = "SQL_moose_rls_";

/** Config mapping ClickHouse setting names to JWT claim names */
export type RowPoliciesConfig = Record<string, string>;

/**
 * Build RowPolicyOptions from a row policies config and a claim-value source.
 * Only sets ClickHouse settings for claims that are present in the source.
 *
 * Missing claims are skipped — if a table's row policy calls getSetting()
 * for a setting that wasn't set, ClickHouse will error. This is correct:
 * it means the JWT is missing a claim that the queried table requires.
 * Tables whose policies don't reference the missing setting are unaffected.
 *
 * @param config  Maps ClickHouse setting name → claim name
 * @param claims  Maps claim name → claim value (e.g., JWT payload or rlsContext)
 * @returns RowPolicyOptions with the shared RLS role and populated settings
 */
export function buildRowPolicyOptionsFromClaims(
  config: RowPoliciesConfig,
  claims: Record<string, unknown>,
): RowPolicyOptions {
  const clickhouse_settings: Record<string, string> = Object.create(null);
  for (const [settingName, claimName] of Object.entries(config)) {
    const value = claims[claimName];
    if (value !== undefined && value !== null) {
      clickhouse_settings[settingName] = String(value);
    }
  }
  return { role: MOOSE_RLS_ROLE, clickhouse_settings };
}
