//! # CLI Commands
//! A module for all the commands that can be run from the CLI

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum Commands {
    /// Builds your project
    #[command(visible_alias = "b")]
    Build {},
    /// Checks the project for non-runtime errors
    #[command(visible_alias = "c")]
    Check {
        #[arg(long, default_value = "false")]
        write_infra_map: bool,
    },
    /// Displays the changes that will be applied to the infrastructure during the next deployment
    /// to production, considering the current state of the project
    #[command(visible_alias = "pl")]
    Plan {
        /// ClickHouse connection URL
        #[arg(long)]
        clickhouse_url: Option<String>,

        /// Output plan as JSON for programmatic use
        #[arg(long)]
        json: bool,
    },

    /// Execute a migration plan against a remote ClickHouse database
    #[command(visible_alias = "mg")]
    Migrate {
        /// ClickHouse connection URL (e.g., clickhouse://user:pass@host:port/database or https://user:pass@host:port/database)
        /// Authentication credentials should be included in the URL
        #[arg(long)]
        clickhouse_url: Option<String>,
    },

    /// View some data from a table
    #[command(visible_alias = "pk")]
    Peek {
        /// Name of the table to peek
        name: String,
        /// Limit the number of rows to view
        #[arg(short, long, default_value = "5")]
        limit: u8,
        /// Output to a file
        #[arg(short, long)]
        file: Option<PathBuf>,
    },
    /// Generates helpers for your data models (i.e. sdk, api tokens)
    #[command(visible_alias = "g")]
    Generate(GenerateArgs),
    /// View the CLI logs
    #[command(visible_alias = "l")]
    Logs {
        /// Follow the logs in real-time
        #[arg(short, long)]
        tail: bool,

        /// Filter logs by a specific string
        #[arg(short, long)]
        filter: Option<String>,
    },
    /// View infrastructure
    Ls {
        /// Filter by infrastructure type (tables, views, sql_resource)
        #[arg(long)]
        _type: Option<String>,

        /// Filter by name (supports partial matching)
        #[arg(long)]
        name: Option<String>,

        /// Output results in JSON format
        #[arg(long, default_value = "false")]
        json: bool,
    },

    /// Manage database schema import
    Db(DbArgs),
    /// Seed data into your project
    #[command(visible_alias = "s")]
    Seed(SeedCommands),
    /// Truncate tables or delete the last N rows
    #[command(visible_alias = "tr")]
    Truncate {
        /// List of table names to target (omit when using --all)
        #[arg(value_name = "TABLE", num_args = 0.., value_delimiter = ',')]
        tables: Vec<String>,

        /// Apply the operation to all tables in the current database
        #[arg(long, conflicts_with = "tables", default_value = "false")]
        all: bool,

        /// Number of most recent rows to delete per table. Omit to delete all rows.
        #[arg(long)]
        rows: Option<u64>,
    },
    /// Execute SQL queries against ClickHouse
    #[command(visible_alias = "q")]
    Query {
        /// SQL query to execute
        query: Option<String>,

        /// Read query from file
        #[arg(short = 'f', long = "file", conflicts_with = "query")]
        file: Option<PathBuf>,

        /// Maximum number of rows to return (applied via ClickHouse settings)
        #[arg(short, long, default_value = "10000")]
        limit: u64,

        /// Format query as code literal (typescript). Skips execution.
        #[arg(short = 'c', long = "format-query", value_name = "LANGUAGE")]
        format_query: Option<String>,

        /// Prettify SQL before formatting (only with --format-query)
        #[arg(short = 'p', long = "prettify", requires = "format_query")]
        prettify: bool,
    },
}

#[derive(Debug, Args)]
pub struct GenerateArgs {
    #[command(subcommand)]
    pub command: Option<GenerateCommand>,
}

#[derive(Debug, Subcommand)]
pub enum GenerateCommand {
    /// Generate an API key hash and bearer token pair for authentication
    #[command(visible_alias = "h")]
    HashToken {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Generate migration files
    #[command(visible_alias = "m")]
    Migration {
        /// ClickHouse connection URL
        #[arg(long)]
        clickhouse_url: Option<String>,

        /// Save the migration files in the migrations/ directory
        #[arg(long, default_value = "false")]
        save: bool,
    },
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct SeedCommands {
    #[command(subcommand)]
    pub command: Option<SeedSubcommands>,
}

#[derive(Debug, Subcommand)]
pub enum SeedSubcommands {
    /// Seed ClickHouse tables with data
    #[command(visible_alias = "c")]
    Clickhouse {
        /// ClickHouse connection URL (e.g. 'clickhouse://explorer@play.clickhouse.com:9440/default')
        #[arg(long, alias = "connection-string")]
        clickhouse_url: Option<String>,
        /// Limit the number of rows to copy per table.
        /// When omitted, falls back to per-table seedFilter.limit, then to 1000.
        #[arg(long, value_name = "LIMIT", conflicts_with = "all")]
        limit: Option<usize>,
        /// Copy all rows (ignore limit). If set for a table, copies entire table.
        #[arg(long, default_value = "false", conflicts_with = "limit")]
        all: bool,
        /// ORDER BY clause of the query. e.g. `--order-by 'timestamp DESC' --limit 10` for the latest 10 rows
        #[arg(long)]
        order_by: Option<String>,
        /// Only seed a specific table (optional)
        #[arg(long, value_name = "TABLE_NAME")]
        table: Option<String>,
        /// Report row counts after seeding. Counts shown for default database only (use --report=false to skip)
        #[arg(long, default_value = "true", action = clap::ArgAction::Set)]
        report: bool,
    },
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct DbArgs {
    #[command(subcommand)]
    pub command: DbCommands,
}

#[derive(Debug, Subcommand)]
pub enum DbCommands {
    /// Update DB schema for EXTERNALLY_MANAGED tables
    #[command(visible_alias = "p")]
    Pull {
        /// ClickHouse connection URL (e.g., clickhouse://user:pass@host:port/database or https://user:pass@host:port/database)
        #[arg(long)]
        clickhouse_url: Option<String>,
        /// File storing the EXTERNALLY_MANAGED table definitions, defaults to app/externalModels.ts
        #[arg(long)]
        file_path: Option<String>,
    },
}
