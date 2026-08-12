import path from "node:path";
import * as toml from "toml";

/**
 * ClickHouse configuration from tch.config.toml
 */
export interface ClickHouseConfig {
  host: string;
  host_port: number;
  user: string;
  password: string;
  db_name: string;
  use_ssl?: boolean;
  native_port?: number;
  rls_user?: string;
  rls_password?: string;
}

/**
 * Redpanda/Kafka configuration from tch.config.toml
 */
export interface KafkaConfig {
  /** Broker connection string (e.g., "host:port" or comma-separated list) */
  broker: string;
  /** Message timeout in milliseconds */
  message_timeout_ms: number;
  /** Default retention period in milliseconds */
  retention_ms: number;
  /** Topic replication factor */
  replication_factor?: number;
  /** SASL username for authentication, if required */
  sasl_username?: string;
  /** SASL password for authentication, if required */
  sasl_password?: string;
  /** SASL mechanism (e.g., "PLAIN", "SCRAM-SHA-256") */
  sasl_mechanism?: string;
  /** Security protocol (e.g., "SASL_SSL", "PLAINTEXT") */
  security_protocol?: string;
  /** Optional namespace used as a prefix for topics */
  namespace?: string;
  /** Optional Confluent Schema Registry URL */
  schema_registry_url?: string;
}

/**
 * Project configuration from tch.config.toml
 */
export interface ProjectConfig {
  language: string;
  clickhouse_config: ClickHouseConfig;
  redpanda_config?: KafkaConfig;
  /**
   * Redpanda/Kafka configuration. Previously named `redpanda_config` in some places.
   * Prefer `kafka_config` but support both for backward compatibility.
   */

  kafka_config?: KafkaConfig;
}

/**
 * Error thrown when configuration cannot be found or parsed
 */
export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ConfigError";
  }
}

/** Current project config file name. */
const PROJECT_CONFIG_FILE = "tch.config.toml";

/**
 * Legacy project config file name, kept as a fallback so projects that have
 * not renamed their config file keep working. Mirrors the precedence in the
 * Rust CLI (`OLD_PROJECT_CONFIG_FILE` in `apps/cli/src/project.rs`): the new
 * name wins when both exist in the same directory.
 */
const OLD_PROJECT_CONFIG_FILE = "moose.config.toml";

/**
 * Walks up the directory tree to find tch.config.toml, falling back to the
 * legacy moose.config.toml in the same directory before ascending further.
 */
async function findConfigFile(
  startDir: string = process.cwd(),
): Promise<string | null> {
  const fs = await import("node:fs");

  let currentDir = path.resolve(startDir);

  while (true) {
    const configPath = path.join(currentDir, PROJECT_CONFIG_FILE);
    if (fs.existsSync(configPath)) {
      return configPath;
    }

    const legacyConfigPath = path.join(currentDir, OLD_PROJECT_CONFIG_FILE);
    if (fs.existsSync(legacyConfigPath)) {
      return legacyConfigPath;
    }

    const parentDir = path.dirname(currentDir);
    if (parentDir === currentDir) {
      // Reached root directory
      break;
    }
    currentDir = parentDir;
  }

  return null;
}

/**
 * Reads and parses the project configuration from tch.config.toml (or the
 * legacy moose.config.toml, if that is all that is present).
 */
export async function readProjectConfig(): Promise<ProjectConfig> {
  const fs = await import("node:fs");
  const configPath = await findConfigFile();
  if (!configPath) {
    throw new ConfigError(
      `${PROJECT_CONFIG_FILE} not found in current directory or any parent directory ` +
        `(legacy ${OLD_PROJECT_CONFIG_FILE} not found either)`,
    );
  }

  try {
    const configContent = fs.readFileSync(configPath, "utf-8");
    const config = toml.parse(configContent) as ProjectConfig;
    return config;
  } catch (error) {
    throw new ConfigError(
      `Failed to parse ${path.basename(configPath)}: ${error}`,
    );
  }
}
