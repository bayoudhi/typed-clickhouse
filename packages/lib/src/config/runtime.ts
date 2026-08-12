import { readProjectConfig } from "./configFile";

interface RuntimeClickHouseConfig {
  host: string;
  port: string;
  username: string;
  password: string;
  database: string;
  useSSL: boolean;
  rlsUser?: string;
  rlsPassword?: string;
}

class ConfigurationRegistry {
  private static instance: ConfigurationRegistry;
  private clickhouseConfig?: RuntimeClickHouseConfig;

  static getInstance(): ConfigurationRegistry {
    if (!ConfigurationRegistry.instance) {
      ConfigurationRegistry.instance = new ConfigurationRegistry();
    }
    return ConfigurationRegistry.instance;
  }

  setClickHouseConfig(config: RuntimeClickHouseConfig): void {
    this.clickhouseConfig = config;
  }

  private _env(name: string): string | undefined {
    const value = process.env[name];
    if (value === undefined) return undefined;
    const trimmed = value.trim();
    return trimmed.length > 0 ? trimmed : undefined;
  }

  private _parseBool(value: string | undefined): boolean | undefined {
    if (value === undefined) return undefined;
    switch (value.trim().toLowerCase()) {
      case "1":
      case "true":
      case "yes":
      case "on":
        return true;
      case "0":
      case "false":
      case "no":
      case "off":
        return false;
      default:
        return undefined;
    }
  }

  async getClickHouseConfig(): Promise<RuntimeClickHouseConfig> {
    if (this.clickhouseConfig) {
      return this.clickhouseConfig;
    }

    // Fallback to reading from config file for backward compatibility
    const projectConfig = await readProjectConfig();
    const envHost = this._env("TCH_CLICKHOUSE_CONFIG__HOST");
    const envPort = this._env("TCH_CLICKHOUSE_CONFIG__HOST_PORT");
    const envUser = this._env("TCH_CLICKHOUSE_CONFIG__USER");
    const envPassword = this._env("TCH_CLICKHOUSE_CONFIG__PASSWORD");
    const envDb = this._env("TCH_CLICKHOUSE_CONFIG__DB_NAME");
    const envUseSSL = this._parseBool(
      this._env("TCH_CLICKHOUSE_CONFIG__USE_SSL"),
    );
    const envRlsUser = this._env("TCH_CLICKHOUSE_CONFIG__RLS_USER");
    const envRlsPassword = this._env("TCH_CLICKHOUSE_CONFIG__RLS_PASSWORD");

    return {
      host: envHost ?? projectConfig.clickhouse_config.host,
      port: envPort ?? projectConfig.clickhouse_config.host_port.toString(),
      username: envUser ?? projectConfig.clickhouse_config.user,
      password: envPassword ?? projectConfig.clickhouse_config.password,
      database: envDb ?? projectConfig.clickhouse_config.db_name,
      useSSL:
        envUseSSL !== undefined ? envUseSSL : (
          projectConfig.clickhouse_config.use_ssl || false
        ),
      rlsUser:
        envRlsUser ?? projectConfig.clickhouse_config.rls_user ?? undefined,
      rlsPassword:
        envRlsPassword ??
        projectConfig.clickhouse_config.rls_password ??
        undefined,
    };
  }

  async getStandaloneClickhouseConfig(
    overrides?: Partial<RuntimeClickHouseConfig>,
  ): Promise<RuntimeClickHouseConfig> {
    if (this.clickhouseConfig) {
      return { ...this.clickhouseConfig, ...overrides };
    }

    const envHost = this._env("TCH_CLICKHOUSE_CONFIG__HOST");
    const envPort = this._env("TCH_CLICKHOUSE_CONFIG__HOST_PORT");
    const envUser = this._env("TCH_CLICKHOUSE_CONFIG__USER");
    const envPassword = this._env("TCH_CLICKHOUSE_CONFIG__PASSWORD");
    const envDb = this._env("TCH_CLICKHOUSE_CONFIG__DB_NAME");
    const envUseSSL = this._parseBool(
      this._env("TCH_CLICKHOUSE_CONFIG__USE_SSL"),
    );
    const envRlsUser = this._env("TCH_CLICKHOUSE_CONFIG__RLS_USER");
    const envRlsPassword = this._env("TCH_CLICKHOUSE_CONFIG__RLS_PASSWORD");

    let projectConfig;
    try {
      projectConfig = await readProjectConfig();
    } catch (error) {
      projectConfig = null;
    }

    const defaults = {
      host: "localhost",
      port: "18123",
      username: "default",
      password: "",
      database: "local",
      useSSL: false,
    };

    return {
      host:
        overrides?.host ??
        envHost ??
        projectConfig?.clickhouse_config.host ??
        defaults.host,
      port:
        overrides?.port ??
        envPort ??
        projectConfig?.clickhouse_config.host_port.toString() ??
        defaults.port,
      username:
        overrides?.username ??
        envUser ??
        projectConfig?.clickhouse_config.user ??
        defaults.username,
      password:
        overrides?.password ??
        envPassword ??
        projectConfig?.clickhouse_config.password ??
        defaults.password,
      database:
        overrides?.database ??
        envDb ??
        projectConfig?.clickhouse_config.db_name ??
        defaults.database,
      useSSL:
        overrides?.useSSL ??
        envUseSSL ??
        projectConfig?.clickhouse_config.use_ssl ??
        defaults.useSSL,
      rlsUser:
        envRlsUser ?? projectConfig?.clickhouse_config.rls_user ?? undefined,
      rlsPassword:
        envRlsPassword ??
        projectConfig?.clickhouse_config.rls_password ??
        undefined,
    };
  }

  hasRuntimeConfig(): boolean {
    return !!this.clickhouseConfig;
  }
}

(globalThis as any)._tchConfigRegistry = ConfigurationRegistry.getInstance();
export type { ConfigurationRegistry, RuntimeClickHouseConfig };
