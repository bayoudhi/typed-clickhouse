/**
 * @fileoverview Serverless-compatible entry point for @typed-clickhouse/lib
 *
 * This module re-exports the ClickHouse-only subset of SDK classes, types,
 * and utilities needed for serverless environments (AWS Lambda, edge
 * runtimes, etc.): table/view/materialized-view definitions, row-level
 * security, and the SQL query layer.
 *
 * Streaming (Stream, DeadLetterQueue), workflows (Task, Workflow) and the
 * HTTP Api/IngestApi/IngestPipeline layer no longer exist anywhere in this
 * fork's source tree -- this fork only ships ClickHouse tooling.
 *
 * No source file under packages/lib/src imports the Kafka client
 * library, Temporal, or Redis (verified by packages/core's
 * tsup bundle and the golden export test).
 *

 * @module serverless
 */

// Re-export type aliases (fully erased at compile time -- no runtime import
// of browserCompatible.ts's value exports, such as Stream/Api/Workflow).
export type { Key, JWT } from "./browserCompatible";

// ClickHouse resources: tables, views, materialized views, SQL resources,
// row-level security, life cycle, and the generic WebApp host.
export {
  ClickHouseEngines,
  Aggregated,
  SimpleAggregated,
  OlapTable,
  OlapConfig,
  S3QueueTableSettings,
  SqlResource,
  View,
  MaterializedView,
  MaterializedViewConfig,
  SelectRowPolicy,
  SelectRowPolicyConfig,
  LifeCycle,
  WebApp,
  WebAppConfig,
  WebAppHandler,
  FrameworkApp,
  // Registry functions
  getTables,
  getTable,
  getSqlResources,
  getSqlResource,
  getWebApps,
  getWebApp,
  getView,
  getViews,
  getMaterializedView,
  getMaterializedViews,
  getSelectRowPolicies,
  getSelectRowPolicy,
} from "./dmv2";

// ClickHouse column-type decorators and numeric/date aliases (pure types --
// erased at compile time, so they carry no runtime dependency).
export {
  ClickHousePrecision,
  ClickHouseDecimal,
  ClickHouseByteSize,
  ClickHouseFixedStringSize,
  ClickHouseFloat,
  ClickHouseInt,
  ClickHouseJson,
  LowCardinality,
  ClickHouseNamedTuple,
  ClickHouseDefault,
  ClickHouseTTL,
  ClickHouseMaterialized,
  ClickHouseAlias,
  WithDefault,
  ClickHouseCodec,
  DateTime,
  DateTime64,
  DateTimeString,
  DateTime64String,
  FixedString,
  Float32,
  Float64,
  Int8,
  Int16,
  Int32,
  Int64,
  UInt8,
  UInt16,
  UInt32,
  UInt64,
  Decimal,
} from "./dataModels/types";
export type {
  Insertable,
  ClickHousePoint,
  ClickHouseRing,
  ClickHouseLineString,
  ClickHouseMultiLineString,
  ClickHousePolygon,
  ClickHouseMultiPolygon,
} from "./dataModels/types";

// Re-export types and utilities from commons (pure TS, no native deps).
// ACKs, MAX_RETRIES_PRODUCER, and RETRY_FACTOR_PRODUCER are leftover
// producer-tuning constants; they carry no native dependency themselves.
export type { CliLogData, Logger } from "./commons";
export {
  ACKs,
  antiCachePath,
  cliLog,
  compilerLog,
  getClickhouseClient,
  getFileName,
  logError,
  MAX_RETRIES,
  MAX_RETRIES_PRODUCER,
  MAX_RETRY_TIME_MS,
  mapTstoJs,
  RETRY_FACTOR_PRODUCER,
  RETRY_INITIAL_TIME_MS,
  rewriteImportExtensions,
} from "./commons";

// SQL/query layer: the `sql` template tag, the `Sql` AST, query rendering,
// the `joinQueries` helper, and the ClickHouse query client.
export * from "./sqlHelpers";
export {
  QueryClient,
  type RowPolicyOptions,
} from "./consumption-apis/query-client";

// Row-level security (RLS): shared role/user/setting-prefix constants and the
// claims-based RowPolicyOptions builder. Native-free (see rls-constants.ts),
// so exporting these here does not pull Temporal/Redis into the bundle.
export {
  MOOSE_RLS_ROLE,
  MOOSE_RLS_USER,
  MOOSE_RLS_SETTING_PREFIX,
  buildRowPolicyOptionsFromClaims,
  type RowPoliciesConfig,
} from "./rls-constants";

// Standalone query utilities: `getHandlerUtils()` returns a ClickHouse-backed
// `ResourceClient` plus the `sql` tag, resolving connection details from the
// configuration registry (`tch.config.toml`, or `configureClickHouse()` in a
// serverless environment). Pass `{ rlsContext }` for a row-policy-scoped
// client. Native-free: the only non-pure imports are `@clickhouse/client`,
// `jose` (types only), and `node:async_hooks` -- available on AWS Lambda, but
// not on edge runtimes that ship a partial Node built-in surface.
export { getHandlerUtils } from "./consumption-apis/standalone";
export type { GetHandlerUtilsOptions } from "./consumption-apis/standalone";
export { ResourceClient } from "./consumption-apis/helpers";
export type { HandlerUtils } from "./consumption-apis/helpers";

// Type-safe SQL query building on top of the SQL layer (defineQueryModel,
// the fluent query builder, filter/aggregation helpers, and MCP tool
// generation for query models).
export * from "./query-layer";

// Export data source connector abstract class (pure TS)
export * from "./connectors/dataSource";
// Export secrets and runtime environment helpers (pure TS, no native deps)
export * from "./secrets";
// Export utility types and helpers (pure TS)
export * from "./utilities";
