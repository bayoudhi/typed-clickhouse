/**
 * @module internal
 * Internal implementation details for the the library v2 data model (dmv2).
 *
 * This module manages the registration of user-defined dmv2 resources (Tables, SQL
 * resources, WebApps, etc.) and provides functions to serialize these resources into
 * a JSON format (`InfrastructureMap`) expected by the the library infrastructure management
 * system. It also includes the base class (`TypedBase`) used by dmv2 resource classes.
 *
 * @internal This module is intended for internal use by the the library library and compiler plugin.
 *           Its API might change without notice.
 */
import process from "process";
import * as path from "path";
import { SqlResource } from "./index";
import { Column } from "../dataModels/dataModelTypes";
import { ClickHouseEngines } from "../dataModels/types";
import {
  OlapTable,
  OlapConfig,
  ReplacingMergeTreeConfig,
  SummingMergeTreeConfig,
  ReplicatedMergeTreeConfig,
  ReplicatedReplacingMergeTreeConfig,
  ReplicatedAggregatingMergeTreeConfig,
  ReplicatedSummingMergeTreeConfig,
  ReplicatedCollapsingMergeTreeConfig,
  ReplicatedVersionedCollapsingMergeTreeConfig,
  S3QueueConfig,
} from "./sdk/olapTable";
import type { TableProjection } from "./sdk/olapTable";
import { compilerLog } from "../commons";
import { WebApp } from "./sdk/webApp";
import { MaterializedView } from "./sdk/materializedView";
import { View } from "./sdk/view";
import { SelectRowPolicy } from "./sdk/selectRowPolicy";
import {
  getSourceDir,
  getCompiledIndexPath,
  getOutDir,
  hasCompiledArtifacts,
  loadModule,
} from "../compiler-config";
import {
  analyzeRegistryLineage,
  type DependencyAnalysisResult,
  type InfrastructureSignatureJson,
} from "./dependencyAnalysis";
import { findSourceFiles } from "./utils";

/**
 * Strips the file extension from a path, returning the "stem".
 * Handles compound extensions like .d.ts by stripping only the last extension.
 */
function pathStem(filePath: string): string {
  const ext = path.extname(filePath);
  return ext ? filePath.slice(0, -ext.length) : filePath;
}

/**
 * Checks for source files that exist but weren't loaded.
 *
 * Since we now load pre-compiled JS from the outDir (e.g. .tch/compiled/app/),
 * require.cache contains compiled paths, not source paths. We compare using
 * path stems relative to each root: for source files relative to appDir, and
 * for loaded files relative to compiledAppDir. This maps e.g.
 *   source:   app/models.ts         -> stem "models"
 *   compiled: .tch/compiled/app/models.js -> stem "models"
 */
function findUnloadedFiles(): string[] {
  const cwd = process.cwd();
  const sourceDir = getSourceDir();
  const appDir = path.resolve(cwd, sourceDir);

  // The compiled equivalent of appDir lives under outDir/sourceDir
  const compiledAppDir = path.resolve(cwd, getOutDir(), sourceDir);

  // Find all source files in the source directory
  const allSourceFiles = findSourceFiles(appDir, (directory, error) => {
    compilerLog(`Warning: Could not read directory ${directory}: ${error}`);
  });

  // Build a set of stems from require.cache entries under the compiled directory.
  // e.g. ".tch/compiled/app/models.js" -> stem "models"
  const loadedStems = new Set(
    Object.keys(require.cache)
      .filter((key) => key.startsWith(compiledAppDir))
      .map((key) => pathStem(path.relative(compiledAppDir, key))),
  );

  // A source file is unloaded if its stem (relative to appDir) is not in loadedStems.
  // e.g. "app/unloaded_table.ts" -> stem "unloaded_table" -> not in loadedStems
  const unloadedFiles = allSourceFiles
    .filter((file) => {
      const stem = pathStem(path.relative(appDir, file));
      return !loadedStems.has(stem);
    })
    .map((file) => path.relative(cwd, file));

  return unloadedFiles;
}

/**
 * Client-only mode check. When true, resource registration is permissive
 * (duplicates overwrite silently instead of throwing).
 * Set via TCH_CLIENT_ONLY=true environment variable.
 *
 * This enables Next.js apps to import OlapTable definitions for type-safe
 * queries without the the library runtime, avoiding "already exists" errors on HMR.
 *
 * @returns true if TCH_CLIENT_ONLY environment variable is set to "true"
 */
export const isClientOnlyMode = (): boolean =>
  process.env.TCH_CLIENT_ONLY === "true";

class MutationTrackingMap<K, V> extends Map<K, V> {
  private onMutate: (() => void) | undefined;

  constructor(entries?: Iterable<readonly [K, V]>, onMutate?: () => void) {
    super(entries);
    this.onMutate = onMutate;
  }

  setMutationListener(onMutate: () => void): void {
    this.onMutate = onMutate;
  }

  override set(key: K, value: V): this {
    super.set(key, value);
    this.onMutate?.();
    return this;
  }

  override delete(key: K): boolean {
    const deleted = super.delete(key);
    if (deleted) {
      this.onMutate?.();
    }
    return deleted;
  }

  override clear(): void {
    if (this.size === 0) {
      return;
    }
    super.clear();
    this.onMutate?.();
  }
}

type InternalRegistry = {
  tables: Map<string, OlapTable<any>>;
  sqlResources: Map<string, SqlResource>;
  webApps: Map<string, WebApp>;
  materializedViews: Map<string, MaterializedView<any>>;
  views: Map<string, View>;
  selectRowPolicies: Map<string, SelectRowPolicy>;
};

let registryMutationVersion = 0;
let lineageCache:
  | {
      registry: InternalRegistry;
      version: number;
      result: DependencyAnalysisResult;
    }
  | undefined;

const markRegistryMutated = () => {
  registryMutationVersion += 1;
  lineageCache = undefined;
};

function toTrackingMap<V>(
  map: Map<string, V> | undefined,
): MutationTrackingMap<string, V> {
  if (map instanceof MutationTrackingMap) {
    map.setMutationListener(markRegistryMutated);
    return map;
  }
  return new MutationTrackingMap<string, V>(
    map?.entries(),
    markRegistryMutated,
  );
}

function createRegistryFrom(
  existing?: Partial<InternalRegistry>,
): InternalRegistry {
  return {
    tables: toTrackingMap(existing?.tables),
    sqlResources: toTrackingMap(existing?.sqlResources),
    webApps: toTrackingMap(existing?.webApps),
    materializedViews: toTrackingMap(existing?.materializedViews),
    views: toTrackingMap(existing?.views),
    selectRowPolicies: toTrackingMap(existing?.selectRowPolicies),
  };
}

/**
 * Internal registry holding all defined the library dmv2 resources.
 * Populated by the constructors of OlapTable, SqlResource, WebApp, etc.
 * Accessed via `getInternalRegistry()`.
 */
const tch_internal: InternalRegistry = {
  tables: new MutationTrackingMap<string, OlapTable<any>>(
    undefined,
    markRegistryMutated,
  ),
  sqlResources: new MutationTrackingMap<string, SqlResource>(
    undefined,
    markRegistryMutated,
  ),
  webApps: new MutationTrackingMap<string, WebApp>(
    undefined,
    markRegistryMutated,
  ),
  materializedViews: new MutationTrackingMap<string, MaterializedView<any>>(
    undefined,
    markRegistryMutated,
  ),
  views: new MutationTrackingMap<string, View>(undefined, markRegistryMutated),
  selectRowPolicies: new MutationTrackingMap<string, SelectRowPolicy>(
    undefined,
    markRegistryMutated,
  ),
};

function getCachedLineage(
  registry: InternalRegistry,
): DependencyAnalysisResult {
  if (
    lineageCache &&
    lineageCache.registry === registry &&
    lineageCache.version === registryMutationVersion
  ) {
    return lineageCache.result;
  }

  const result = analyzeRegistryLineage(registry);
  lineageCache = {
    registry,
    version: registryMutationVersion,
    result,
  };
  return result;
}
/**
 * Engine-specific configuration types using discriminated union pattern
 */
interface MergeTreeEngineConfig {
  engine: "MergeTree";
}

interface ReplacingMergeTreeEngineConfig {
  engine: "ReplacingMergeTree";
  ver?: string;
  isDeleted?: string;
}

interface AggregatingMergeTreeEngineConfig {
  engine: "AggregatingMergeTree";
}

interface SummingMergeTreeEngineConfig {
  engine: "SummingMergeTree";
  columns?: string[];
}

interface CollapsingMergeTreeEngineConfig {
  engine: "CollapsingMergeTree";
  sign: string;
}

interface VersionedCollapsingMergeTreeEngineConfig {
  engine: "VersionedCollapsingMergeTree";
  sign: string;
  ver: string;
}

interface ReplicatedMergeTreeEngineConfig {
  engine: "ReplicatedMergeTree";
  keeperPath?: string;
  replicaName?: string;
}

interface ReplicatedReplacingMergeTreeEngineConfig {
  engine: "ReplicatedReplacingMergeTree";
  keeperPath?: string;
  replicaName?: string;
  ver?: string;
  isDeleted?: string;
}

interface ReplicatedAggregatingMergeTreeEngineConfig {
  engine: "ReplicatedAggregatingMergeTree";
  keeperPath?: string;
  replicaName?: string;
}

interface ReplicatedSummingMergeTreeEngineConfig {
  engine: "ReplicatedSummingMergeTree";
  keeperPath?: string;
  replicaName?: string;
  columns?: string[];
}

interface ReplicatedCollapsingMergeTreeEngineConfig {
  engine: "ReplicatedCollapsingMergeTree";
  keeperPath?: string;
  replicaName?: string;
  sign: string;
}

interface ReplicatedVersionedCollapsingMergeTreeEngineConfig {
  engine: "ReplicatedVersionedCollapsingMergeTree";
  keeperPath?: string;
  replicaName?: string;
  sign: string;
  ver: string;
}

interface S3QueueEngineConfig {
  engine: "S3Queue";
  s3Path: string;
  format: string;
  awsAccessKeyId?: string;
  awsSecretAccessKey?: string;
  compression?: string;
  headers?: { [key: string]: string };
}

interface S3EngineConfig {
  engine: "S3";
  path: string;
  format: string;
  awsAccessKeyId?: string;
  awsSecretAccessKey?: string;
  compression?: string;
  partitionStrategy?: string;
  partitionColumnsInDataFile?: string;
}

interface BufferEngineConfig {
  engine: "Buffer";
  targetDatabase: string;
  targetTable: string;
  numLayers: number;
  minTime: number;
  maxTime: number;
  minRows: number;
  maxRows: number;
  minBytes: number;
  maxBytes: number;
  flushTime?: number;
  flushRows?: number;
  flushBytes?: number;
}

interface DistributedEngineConfig {
  engine: "Distributed";
  cluster: string;
  targetDatabase: string;
  targetTable: string;
  shardingKey?: string;
  policyName?: string;
}

interface IcebergS3EngineConfig {
  engine: "IcebergS3";
  path: string;
  format: string;
  awsAccessKeyId?: string;
  awsSecretAccessKey?: string;
  compression?: string;
}

interface KafkaEngineConfig {
  engine: "Kafka";
  brokerList: string;
  topicList: string;
  groupName: string;
  format: string;
}

interface MergeEngineConfig {
  engine: "Merge";
  sourceDatabase: string;
  tablesRegexp: string;
}

/**
 * Union type for all supported engine configurations
 */
type EngineConfig =
  | MergeTreeEngineConfig
  | ReplacingMergeTreeEngineConfig
  | AggregatingMergeTreeEngineConfig
  | SummingMergeTreeEngineConfig
  | CollapsingMergeTreeEngineConfig
  | VersionedCollapsingMergeTreeEngineConfig
  | ReplicatedMergeTreeEngineConfig
  | ReplicatedReplacingMergeTreeEngineConfig
  | ReplicatedAggregatingMergeTreeEngineConfig
  | ReplicatedSummingMergeTreeEngineConfig
  | ReplicatedCollapsingMergeTreeEngineConfig
  | ReplicatedVersionedCollapsingMergeTreeEngineConfig
  | S3QueueEngineConfig
  | S3EngineConfig
  | BufferEngineConfig
  | DistributedEngineConfig
  | IcebergS3EngineConfig
  | KafkaEngineConfig
  | MergeEngineConfig;

/**
 * JSON representation of an OLAP table configuration.
 */
interface TableJson {
  /** The name of the table. */
  name: string;
  /** Array defining the table's columns and their types. */
  columns: Column[];
  /** ORDER BY clause: either array of column names or a single ClickHouse expression. */
  orderBy: string[] | string;
  /** The column name used for the PARTITION BY clause. */
  partitionBy?: string;
  /** SAMPLE BY expression for approximate query processing. */
  sampleByExpression?: string;
  /** PRIMARY KEY expression (overrides column-level primary_key flags when specified). */
  primaryKeyExpression?: string;
  /** Engine configuration with type-safe, engine-specific parameters */
  engineConfig?: EngineConfig;
  /** Optional version string for the table configuration. */
  version?: string;
  /** Optional metadata for the table (e.g., description). */
  metadata?: { description?: string };
  /** Lifecycle management setting for the table. */
  lifeCycle?: string;
  /** Optional table-level settings that can be modified with ALTER TABLE MODIFY SETTING. */
  tableSettings?: { [key: string]: string };
  /** Optional table indexes */
  indexes?: {
    name: string;
    expression: string;
    type: string;
    arguments: string[];
    granularity: number;
  }[];
  /** Optional table projections */
  projections?: TableProjection[];
  /** Optional table-level TTL expression (without leading 'TTL'). */
  ttl?: string;
  /** Optional database name for multi-database support. */
  database?: string;
  /** Optional cluster name for ON CLUSTER support. */
  cluster?: string;
  /** Optional seed filter for `tch seed clickhouse`. */
  seedFilter?: { limit?: number; where?: string };
}
interface WebAppJson {
  name: string;
  mountPath: string;
  metadata?: { description?: string };
  pullsDataFrom: InfrastructureSignatureJson[];
  pushesDataTo: InfrastructureSignatureJson[];
}

interface SqlResourceJson {
  /** The name of the SQL resource. */
  name: string;
  /** Array of SQL DDL statements required to create the resource. */
  setup: readonly string[];
  /** Array of SQL DDL statements required to drop the resource. */
  teardown: readonly string[];

  /** List of infrastructure components (by signature) that this resource reads from. */
  pullsDataFrom: InfrastructureSignatureJson[];
  /** List of infrastructure components (by signature) that this resource writes to. */
  pushesDataTo: InfrastructureSignatureJson[];
  /** Optional source file path where this resource is defined. */
  sourceFile?: string;
  /** Optional source line number where this resource is defined. */
  sourceLine?: number;
  /** Optional source column number where this resource is defined. */
  sourceColumn?: number;
}

/**
 * JSON representation of a structured Materialized View.
 */
interface MaterializedViewJson {
  /** Name of the materialized view */
  name: string;
  /** Database where the MV is created (optional, uses default if not set) */
  database?: string;
  /** The SELECT SQL statement */
  selectSql: string;
  /** Source tables that the SELECT reads from */
  sourceTables: string[];
  /** Target table where transformed data is written */
  targetTable: string;
  /** Target table database (optional) */
  targetDatabase?: string;
  /** Optional metadata for the materialized view (e.g., description, source file) */
  metadata?: { [key: string]: any };
  /** Optional lifecycle management policy */
  lifeCycle?: string;
}

/**
 * JSON representation of a structured View.
 */
/**
 * JSON representation of a SelectRowPolicy.
 */
interface SelectRowPolicyJson {
  /** Name of the row policy */
  name: string;
  /** Tables the policy applies to */
  tables: { name: string; database?: string }[];
  /** Column to filter on */
  column: string;
  /** JWT claim name for the filter value */
  claim: string;
}

interface ViewJson {
  /** Name of the view */
  name: string;
  /** Database where the view is created (optional, uses default if not set) */
  database?: string;
  /** The SELECT SQL statement */
  selectSql: string;
  /** Source tables that the SELECT reads from */
  sourceTables: string[];
  /** Optional metadata for the view (e.g., description, source file) */
  metadata?: { [key: string]: any };
}

/**
 * Type guard: Check if config is S3QueueConfig
 */
function isS3QueueConfig(
  config: OlapConfig<any>,
): config is S3QueueConfig<any> {
  return "engine" in config && config.engine === ClickHouseEngines.S3Queue;
}

/**
 * Type guard: Check if config has a replicated engine
 * Checks if the engine value is one of the replicated engine types
 */
function hasReplicatedEngine(
  config: OlapConfig<any>,
): config is
  | ReplicatedMergeTreeConfig<any>
  | ReplicatedReplacingMergeTreeConfig<any>
  | ReplicatedAggregatingMergeTreeConfig<any>
  | ReplicatedSummingMergeTreeConfig<any>
  | ReplicatedCollapsingMergeTreeConfig<any>
  | ReplicatedVersionedCollapsingMergeTreeConfig<any> {
  if (!("engine" in config)) {
    return false;
  }

  const engine = config.engine as ClickHouseEngines;
  // Check if engine is one of the replicated engine types
  return (
    engine === ClickHouseEngines.ReplicatedMergeTree ||
    engine === ClickHouseEngines.ReplicatedReplacingMergeTree ||
    engine === ClickHouseEngines.ReplicatedAggregatingMergeTree ||
    engine === ClickHouseEngines.ReplicatedSummingMergeTree ||
    engine === ClickHouseEngines.ReplicatedCollapsingMergeTree ||
    engine === ClickHouseEngines.ReplicatedVersionedCollapsingMergeTree
  );
}

/**
 * Extract engine value from table config, handling both legacy and new formats
 */
function extractEngineValue(config: OlapConfig<any>): ClickHouseEngines {
  // Legacy config without engine property defaults to MergeTree
  if (!("engine" in config)) {
    return ClickHouseEngines.MergeTree;
  }

  // All engines (replicated and non-replicated) have engine as direct value
  return config.engine as ClickHouseEngines;
}

/**
 * Convert engine config for basic MergeTree engines
 */
function convertBasicEngineConfig(
  engine: ClickHouseEngines,
  config: OlapConfig<any>,
): EngineConfig | undefined {
  switch (engine) {
    case ClickHouseEngines.MergeTree:
      return { engine: "MergeTree" };

    case ClickHouseEngines.AggregatingMergeTree:
      return { engine: "AggregatingMergeTree" };

    case ClickHouseEngines.ReplacingMergeTree: {
      const replacingConfig = config as ReplacingMergeTreeConfig<any>;
      return {
        engine: "ReplacingMergeTree",
        ver: replacingConfig.ver,
        isDeleted: replacingConfig.isDeleted,
      };
    }

    case ClickHouseEngines.SummingMergeTree: {
      const summingConfig = config as SummingMergeTreeConfig<any>;
      return {
        engine: "SummingMergeTree",
        columns: summingConfig.columns,
      };
    }

    case ClickHouseEngines.CollapsingMergeTree: {
      const collapsingConfig = config as any; // CollapsingMergeTreeConfig<any>
      return {
        engine: "CollapsingMergeTree",
        sign: collapsingConfig.sign,
      };
    }

    case ClickHouseEngines.VersionedCollapsingMergeTree: {
      const versionedConfig = config as any; // VersionedCollapsingMergeTreeConfig<any>
      return {
        engine: "VersionedCollapsingMergeTree",
        sign: versionedConfig.sign,
        ver: versionedConfig.ver,
      };
    }

    default:
      return undefined;
  }
}

/**
 * Convert engine config for replicated MergeTree engines
 */
function convertReplicatedEngineConfig(
  engine: ClickHouseEngines,
  config: OlapConfig<any>,
): EngineConfig | undefined {
  // First check if this is a replicated engine config
  if (!hasReplicatedEngine(config)) {
    return undefined;
  }

  switch (engine) {
    case ClickHouseEngines.ReplicatedMergeTree: {
      const replicatedConfig = config as ReplicatedMergeTreeConfig<any>;
      return {
        engine: "ReplicatedMergeTree",
        keeperPath: replicatedConfig.keeperPath,
        replicaName: replicatedConfig.replicaName,
      };
    }

    case ClickHouseEngines.ReplicatedReplacingMergeTree: {
      const replicatedConfig =
        config as ReplicatedReplacingMergeTreeConfig<any>;
      return {
        engine: "ReplicatedReplacingMergeTree",
        keeperPath: replicatedConfig.keeperPath,
        replicaName: replicatedConfig.replicaName,
        ver: replicatedConfig.ver,
        isDeleted: replicatedConfig.isDeleted,
      };
    }

    case ClickHouseEngines.ReplicatedAggregatingMergeTree: {
      const replicatedConfig =
        config as ReplicatedAggregatingMergeTreeConfig<any>;
      return {
        engine: "ReplicatedAggregatingMergeTree",
        keeperPath: replicatedConfig.keeperPath,
        replicaName: replicatedConfig.replicaName,
      };
    }

    case ClickHouseEngines.ReplicatedSummingMergeTree: {
      const replicatedConfig = config as ReplicatedSummingMergeTreeConfig<any>;
      return {
        engine: "ReplicatedSummingMergeTree",
        keeperPath: replicatedConfig.keeperPath,
        replicaName: replicatedConfig.replicaName,
        columns: replicatedConfig.columns,
      };
    }

    case ClickHouseEngines.ReplicatedCollapsingMergeTree: {
      const replicatedConfig = config as any; // ReplicatedCollapsingMergeTreeConfig<any>
      return {
        engine: "ReplicatedCollapsingMergeTree",
        keeperPath: replicatedConfig.keeperPath,
        replicaName: replicatedConfig.replicaName,
        sign: replicatedConfig.sign,
      };
    }

    case ClickHouseEngines.ReplicatedVersionedCollapsingMergeTree: {
      const replicatedConfig = config as any; // ReplicatedVersionedCollapsingMergeTreeConfig<any>
      return {
        engine: "ReplicatedVersionedCollapsingMergeTree",
        keeperPath: replicatedConfig.keeperPath,
        replicaName: replicatedConfig.replicaName,
        sign: replicatedConfig.sign,
        ver: replicatedConfig.ver,
      };
    }

    default:
      return undefined;
  }
}

/**
 * Convert S3Queue engine config
 * Uses type guard for fully type-safe property access
 */
function convertS3QueueEngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  if (!isS3QueueConfig(config)) {
    return undefined;
  }

  return {
    engine: "S3Queue",
    s3Path: config.s3Path,
    format: config.format,
    awsAccessKeyId: config.awsAccessKeyId,
    awsSecretAccessKey: config.awsSecretAccessKey,
    compression: config.compression,
    headers: config.headers,
  };
}

/**
 * Convert S3 engine config
 */
function convertS3EngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  if (!("engine" in config) || config.engine !== ClickHouseEngines.S3) {
    return undefined;
  }

  return {
    engine: "S3",
    path: config.path,
    format: config.format,
    awsAccessKeyId: config.awsAccessKeyId,
    awsSecretAccessKey: config.awsSecretAccessKey,
    compression: config.compression,
    partitionStrategy: config.partitionStrategy,
    partitionColumnsInDataFile: config.partitionColumnsInDataFile,
  };
}

/**
 * Convert Buffer engine config
 */
function convertBufferEngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  if (!("engine" in config) || config.engine !== ClickHouseEngines.Buffer) {
    return undefined;
  }

  return {
    engine: "Buffer",
    targetDatabase: config.targetDatabase,
    targetTable: config.targetTable,
    numLayers: config.numLayers,
    minTime: config.minTime,
    maxTime: config.maxTime,
    minRows: config.minRows,
    maxRows: config.maxRows,
    minBytes: config.minBytes,
    maxBytes: config.maxBytes,
    flushTime: config.flushTime,
    flushRows: config.flushRows,
    flushBytes: config.flushBytes,
  };
}

/**
 * Convert Distributed engine config
 */
function convertDistributedEngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  if (
    !("engine" in config) ||
    config.engine !== ClickHouseEngines.Distributed
  ) {
    return undefined;
  }

  return {
    engine: "Distributed",
    cluster: config.cluster,
    targetDatabase: config.targetDatabase,
    targetTable: config.targetTable,
    shardingKey: config.shardingKey,
    policyName: config.policyName,
  };
}

/**
 * Convert IcebergS3 engine config
 */
function convertIcebergS3EngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  if (!("engine" in config) || config.engine !== ClickHouseEngines.IcebergS3) {
    return undefined;
  }

  return {
    engine: "IcebergS3",
    path: config.path,
    format: config.format,
    awsAccessKeyId: config.awsAccessKeyId,
    awsSecretAccessKey: config.awsSecretAccessKey,
    compression: config.compression,
  };
}

/**
 * Convert Kafka engine configuration
 */
function convertKafkaEngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  if (!("engine" in config) || config.engine !== ClickHouseEngines.Kafka) {
    return undefined;
  }

  return {
    engine: "Kafka",
    brokerList: config.brokerList,
    topicList: config.topicList,
    groupName: config.groupName,
    format: config.format,
  };
}

/**
 * Convert Merge engine config
 */
function convertMergeEngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  if (!("engine" in config) || config.engine !== ClickHouseEngines.Merge) {
    return undefined;
  }

  return {
    engine: "Merge",
    sourceDatabase: config.sourceDatabase,
    tablesRegexp: config.tablesRegexp,
  };
}

/**
 * Convert table configuration to engine config
 */
function convertTableConfigToEngineConfig(
  config: OlapConfig<any>,
): EngineConfig | undefined {
  const engine = extractEngineValue(config);

  // Try basic engines first
  const basicConfig = convertBasicEngineConfig(engine, config);
  if (basicConfig) {
    return basicConfig;
  }

  // Try replicated engines
  const replicatedConfig = convertReplicatedEngineConfig(engine, config);
  if (replicatedConfig) {
    return replicatedConfig;
  }

  // Handle S3Queue
  if (engine === ClickHouseEngines.S3Queue) {
    return convertS3QueueEngineConfig(config);
  }

  // Handle S3
  if (engine === ClickHouseEngines.S3) {
    return convertS3EngineConfig(config);
  }

  // Handle Buffer
  if (engine === ClickHouseEngines.Buffer) {
    return convertBufferEngineConfig(config);
  }

  // Handle Distributed
  if (engine === ClickHouseEngines.Distributed) {
    return convertDistributedEngineConfig(config);
  }

  // Handle IcebergS3
  if (engine === ClickHouseEngines.IcebergS3) {
    return convertIcebergS3EngineConfig(config);
  }

  // Handle Kafka
  if (engine === ClickHouseEngines.Kafka) {
    return convertKafkaEngineConfig(config);
  }

  // Handle Merge
  if (engine === ClickHouseEngines.Merge) {
    return convertMergeEngineConfig(config);
  }

  return undefined;
}

export const toInfraMap = (registry: InternalRegistry) => {
  const tables: { [key: string]: TableJson } = {};
  // Streams, Ingest APIs, consumption APIs, and Workflows no longer exist in
  // this fork. These keys stay in the InfrastructureMap (always empty) so the
  // JSON shape consumed by the Rust CLI is unchanged.
  const topics: { [key: string]: never } = {};
  const ingestApis: { [key: string]: never } = {};
  const apis: { [key: string]: never } = {};
  const workflows: { [key: string]: never } = {};
  const sqlResources: { [key: string]: SqlResourceJson } = {};
  const webApps: { [key: string]: WebAppJson } = {};
  const materializedViews: { [key: string]: MaterializedViewJson } = {};
  const views: { [key: string]: ViewJson } = {};
  const selectRowPolicies: { [key: string]: SelectRowPolicyJson } = {};
  const lineage = getCachedLineage(registry);

  registry.tables.forEach((table) => {
    const id =
      table.config.version ?
        `${table.name}_${table.config.version}`
      : table.name;
    // If the table is part of an IngestPipeline, inherit metadata if not set
    let metadata = (table as any).metadata;
    if (!metadata && table.config && (table as any).pipelineParent) {
      metadata = (table as any).pipelineParent.metadata;
    }
    // Create type-safe engine configuration
    const engineConfig: EngineConfig | undefined =
      convertTableConfigToEngineConfig(table.config);

    // Get table settings, applying defaults for S3Queue
    let tableSettings: { [key: string]: string } | undefined = undefined;

    if (table.config.settings) {
      // Convert all settings to strings, filtering out undefined values
      tableSettings = Object.entries(table.config.settings).reduce(
        (acc, [key, value]) => {
          if (value !== undefined) {
            acc[key] = String(value);
          }
          return acc;
        },
        {} as { [key: string]: string },
      );
    }

    // Apply default settings for S3Queue if not already specified
    if (engineConfig?.engine === "S3Queue") {
      if (!tableSettings) {
        tableSettings = {};
      }
      // Set default mode to 'unordered' if not specified
      if (!tableSettings.mode) {
        tableSettings.mode = "unordered";
      }
    }

    // Determine ORDER BY from config
    // Note: engines like Buffer and Distributed don't support orderBy/partitionBy/sampleBy
    const hasOrderByFields =
      "orderByFields" in table.config &&
      Array.isArray(table.config.orderByFields) &&
      table.config.orderByFields.length > 0;
    const hasOrderByExpression =
      "orderByExpression" in table.config &&
      typeof table.config.orderByExpression === "string" &&
      table.config.orderByExpression.length > 0;
    if (hasOrderByFields && hasOrderByExpression) {
      throw new Error(
        `Table ${table.name}: Provide either orderByFields or orderByExpression, not both.`,
      );
    }
    const orderBy: string[] | string =
      hasOrderByExpression && "orderByExpression" in table.config ?
        (table.config.orderByExpression ?? "")
      : "orderByFields" in table.config ? (table.config.orderByFields ?? [])
      : [];

    tables[id] = {
      name: table.name,
      columns: table.columnArray,
      orderBy,
      partitionBy:
        "partitionBy" in table.config ? table.config.partitionBy : undefined,
      sampleByExpression:
        "sampleByExpression" in table.config ?
          table.config.sampleByExpression
        : undefined,
      primaryKeyExpression:
        "primaryKeyExpression" in table.config ?
          table.config.primaryKeyExpression
        : undefined,
      engineConfig,
      version: table.config.version,
      metadata,
      lifeCycle: table.config.lifeCycle,
      // Map 'settings' to 'tableSettings' for internal use
      tableSettings:
        tableSettings && Object.keys(tableSettings).length > 0 ?
          tableSettings
        : undefined,
      indexes:
        table.config.indexes?.map((i) => ({
          ...i,
          granularity: i.granularity === undefined ? 1 : i.granularity,
          arguments: i.arguments === undefined ? [] : i.arguments,
        })) || [],
      projections:
        ("projections" in table.config && table.config.projections) || [],
      ttl: table.config.ttl,
      database: table.config.database,
      cluster: table.config.cluster,
      seedFilter:
        "seedFilter" in table.config ? table.config.seedFilter : undefined,
    };
  });

  registry.sqlResources.forEach((sqlResource) => {
    sqlResources[sqlResource.name] = {
      name: sqlResource.name,
      setup: sqlResource.setup,
      teardown: sqlResource.teardown,
      sourceFile: sqlResource.sourceFile,
      sourceLine: sqlResource.sourceLine,
      sourceColumn: sqlResource.sourceColumn,

      pullsDataFrom: sqlResource.pullsDataFrom.map((r) => {
        if (r.kind === "OlapTable") {
          const table = r as OlapTable<any>;
          const id =
            table.config.version ?
              `${table.name}_${table.config.version}`
            : table.name;
          return {
            id,
            kind: "Table",
          };
        } else if (r.kind === "SqlResource") {
          const resource = r as SqlResource;
          return {
            id: resource.name,
            kind: "SqlResource",
          };
        } else if (r.kind === "View") {
          const view = r as View;
          return {
            id: view.name,
            kind: "View",
          };
        } else if (r.kind === "MaterializedView") {
          const mv = r as MaterializedView<any>;
          return {
            id: mv.name,
            kind: "MaterializedView",
          };
        } else {
          throw new Error(`Unknown sql resource dependency type: ${r}`);
        }
      }),
      pushesDataTo: sqlResource.pushesDataTo.map((r) => {
        if (r.kind === "OlapTable") {
          const table = r as OlapTable<any>;
          const id =
            table.config.version ?
              `${table.name}_${table.config.version}`
            : table.name;
          return {
            id,
            kind: "Table",
          };
        } else if (r.kind === "SqlResource") {
          const resource = r as SqlResource;
          return {
            id: resource.name,
            kind: "SqlResource",
          };
        } else if (r.kind === "View") {
          const view = r as View;
          return {
            id: view.name,
            kind: "View",
          };
        } else if (r.kind === "MaterializedView") {
          const mv = r as MaterializedView<any>;
          return {
            id: mv.name,
            kind: "MaterializedView",
          };
        } else {
          throw new Error(`Unknown sql resource dependency type: ${r}`);
        }
      }),
    };
  });

  registry.webApps.forEach((webApp) => {
    const webAppLineage = lineage.webAppByName.get(webApp.name);
    webApps[webApp.name] = {
      name: webApp.name,
      mountPath: webApp.config.mountPath || "/",
      metadata: webApp.config.metadata,
      pullsDataFrom: webAppLineage?.pullsDataFrom ?? [],
      pushesDataTo: webAppLineage?.pushesDataTo ?? [],
    };
  });

  // Serialize materialized views with structured data
  registry.materializedViews.forEach((mv) => {
    materializedViews[mv.name] = {
      name: mv.name,
      selectSql: mv.selectSql,
      sourceTables: mv.sourceTables,
      targetTable: mv.targetTable.name,
      targetDatabase: mv.targetTable.config.database,
      metadata: mv.metadata,
      lifeCycle: mv.lifeCycle,
    };
  });

  // Serialize views with structured data
  registry.views.forEach((view) => {
    views[view.name] = {
      name: view.name,
      selectSql: view.selectSql,
      sourceTables: view.sourceTables,
      metadata: view.metadata,
    };
  });

  registry.selectRowPolicies.forEach((policy) => {
    selectRowPolicies[policy.name] = {
      name: policy.name,
      tables: policy.tableRefs,
      column: policy.config.column,
      claim: policy.config.claim,
    };
  });

  return {
    topics,
    tables,
    ingestApis,
    apis,
    sqlResources,
    workflows,
    webApps,
    materializedViews,
    views,
    selectRowPolicies,
    unloadedFiles: [] as string[], // Will be populated by dumpInternalRegistry
  };
};

/**
 * Retrieves the global internal library resource registry.
 * Uses `globalThis` to ensure a single registry instance.
 *
 * @returns The internal library resource registry.
 */
const initializeInternalRegistry = () => {
  const existing = (globalThis as any).tch_internal as
    | Partial<InternalRegistry>
    | undefined;

  if (existing === undefined) {
    (globalThis as any).tch_internal = tch_internal;
    return;
  }

  (globalThis as any).tch_internal = createRegistryFrom(existing);
};

initializeInternalRegistry();

export const getInternalRegistry = (): InternalRegistry =>
  (globalThis as any).tch_internal;

/**
 * Loads the user's application entry point (`app/index.ts`) to register resources,
 * then generates and prints the infrastructure map as JSON.
 *
 * This function is the main entry point used by the the library infrastructure system
 * to discover the defined resources.
 * It prints the JSON map surrounded by specific delimiters (`___TCH_INFRA_MAP___start`
 * and `end___TCH_INFRA_MAP___`) for easy extraction by the calling process.
 */
export const dumpInternalRegistry = async () => {
  await loadIndex();

  const infraMap = toInfraMap(getInternalRegistry());

  // Check for unloaded files
  const unloadedFiles = findUnloadedFiles();
  infraMap.unloadedFiles = unloadedFiles;

  console.log(
    "___TCH_INFRA_MAP___start",
    JSON.stringify(infraMap),
    "end___TCH_INFRA_MAP___",
  );
};

const loadIndex = async () => {
  // Always use pre-compiled JavaScript - no ts-node fallback.
  // Compilation is handled by tch-tspc before this runs.

  // Check if compiled artifacts exist
  if (!hasCompiledArtifacts()) {
    const outDir = getOutDir();
    const sourceDir = getSourceDir();
    throw new Error(
      `Compiled artifacts not found at ${outDir}/${sourceDir}/index.js. ` +
        `Run 'npx tch-tspc' to compile your TypeScript first.`,
    );
  }

  // Clear registry and require.cache for hot reloading
  const registry = getInternalRegistry();
  registry.tables.clear();
  registry.sqlResources.clear();
  registry.webApps.clear();
  registry.materializedViews.clear();
  registry.views.clear();
  registry.selectRowPolicies.clear();

  // Clear require cache for compiled directory to pick up changes
  const outDir = getOutDir();
  const compiledDir =
    path.isAbsolute(outDir) ? outDir : path.join(process.cwd(), outDir);
  Object.keys(require.cache).forEach((key) => {
    if (key.startsWith(compiledDir)) {
      delete require.cache[key];
    }
  });

  try {
    // Load pre-compiled JavaScript from the configured outDir
    const indexPath = getCompiledIndexPath();
    await loadModule(indexPath);
  } catch (error) {
    let hint: string | undefined;
    let includeDetails = true;
    const details = error instanceof Error ? error.message : String(error);

    // Check for typia configuration errors
    if (
      details.includes("no transform has been configured") ||
      details.includes("NoTransformConfigurationError")
    ) {
      hint =
        "🔴 Typia Transformation Error\n\n" +
        "This is likely a bug in the library. The Typia type transformer failed to process your code.\n\n" +
        "Please report this issue with the stack trace below and the file " +
        "being processed.\n\n";
      includeDetails = false;
    } else if (
      details.includes("ERR_REQUIRE_ESM") ||
      details.includes("ES Module")
    ) {
      hint =
        "The file or its dependencies are ESM-only. Switch to packages that dual-support CJS & ESM, or upgrade to Node 22.12+. " +
        "If you must use Node 20, you may try Node 20.19\n\n";
    }

    if (hint === undefined) {
      throw error;
    } else {
      const errorMsg = includeDetails ? `${hint}${details}` : hint;
      const cause = error instanceof Error ? error : undefined;
      throw new Error(errorMsg, { cause });
    }
  }
};

export const getWebApps = async () => {
  await loadIndex();
  return getInternalRegistry().webApps;
};
