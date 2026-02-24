use clap::{Parser, Subcommand, ValueEnum};

/// Output format for list commands.
#[derive(Debug, Clone, ValueEnum, Default)]
pub enum ListFormat {
    /// Default human-readable table format
    #[default]
    Default,
    /// JSON array output
    Json,
    /// YAML output
    Yaml,
    /// CSV with header row
    Csv,
    /// Tab-separated values with header row
    Tsv,
}

/// chbackup - Drop-in Rust replacement for clickhouse-backup.
/// Single static binary, S3-only storage, non-destructive restore.
#[derive(Parser, Debug)]
#[command(name = "chbackup", version, about)]
pub struct Cli {
    /// Config file path
    #[arg(
        short = 'c',
        long = "config",
        default_value = "/etc/chbackup/config.yml",
        env = "CHBACKUP_CONFIG",
        global = true
    )]
    pub config: String,

    /// Override config params via CLI: --env KEY=VALUE
    #[arg(long = "env", global = true)]
    pub env_overrides: Vec<String>,

    #[command(subcommand)]
    pub command: Command,
}

/// Location specifier for list, delete, and clean_broken commands.
#[derive(Debug, Clone, ValueEnum)]
pub enum Location {
    Local,
    Remote,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a local backup
    Create {
        /// Table filter pattern (globs: db.*, *.table)
        #[arg(short = 't', long = "tables")]
        tables: Option<String>,

        /// Filter by partition names
        #[arg(long)]
        partitions: Option<String>,

        /// Local incremental base backup name
        #[arg(long = "diff-from")]
        diff_from: Option<String>,

        /// Glob patterns for projections to skip
        #[arg(long = "skip-projections")]
        skip_projections: Option<String>,

        /// Schema only (no data)
        #[arg(long)]
        schema: bool,

        /// Include RBAC objects (users, roles, quotas, etc.)
        #[arg(long)]
        rbac: bool,

        /// Include ClickHouse server config files
        #[arg(long)]
        configs: bool,

        /// Include Named Collections
        #[arg(long = "named-collections")]
        named_collections: bool,

        /// Allow backup with inconsistent column types across parts
        #[arg(long = "skip-check-parts-columns")]
        skip_check_parts_columns: bool,

        /// Optional backup name (auto-generated if omitted)
        backup_name: Option<String>,
    },

    /// Upload a local backup to S3
    Upload {
        /// Delete local backup after successful upload
        #[arg(long = "delete-local")]
        delete_local: bool,

        /// Remote incremental base backup name
        #[arg(long = "diff-from-remote")]
        diff_from_remote: Option<String>,

        /// Resume interrupted operation from state file
        #[arg(long)]
        resume: bool,

        /// Optional backup name
        backup_name: Option<String>,
    },

    /// Download a remote backup from S3
    Download {
        /// Deduplicate local parts via hardlinks
        #[arg(long = "hardlink-exists-files")]
        hardlink_exists_files: bool,

        /// Resume interrupted operation from state file
        #[arg(long)]
        resume: bool,

        /// Optional backup name
        backup_name: Option<String>,
    },

    /// Restore a backup to ClickHouse
    Restore {
        /// Table filter pattern (globs: db.*, *.table)
        #[arg(short = 't', long = "tables")]
        tables: Option<String>,

        /// Rename single table: -t db.src --as=db.dst
        #[arg(long = "as", name = "rename")]
        rename_as: Option<String>,

        /// Bulk database remap: -m prod:staging,logs:logs_copy
        #[arg(short = 'm', long = "database-mapping")]
        database_mapping: Option<String>,

        /// Filter by partition names
        #[arg(long)]
        partitions: Option<String>,

        /// Schema only (no data)
        #[arg(long, conflicts_with = "data_only")]
        schema: bool,

        /// Data only (no schema)
        #[arg(long = "data-only")]
        data_only: bool,

        /// DROP existing tables before restore
        #[arg(long = "rm", visible_alias = "drop")]
        rm: bool,

        /// Resume interrupted operation from state file
        #[arg(long)]
        resume: bool,

        /// Include RBAC objects (users, roles, quotas, etc.)
        #[arg(long)]
        rbac: bool,

        /// Include ClickHouse server config files
        #[arg(long)]
        configs: bool,

        /// Include Named Collections
        #[arg(long = "named-collections")]
        named_collections: bool,

        /// Skip restoring tables that have zero data parts
        #[arg(long = "skip-empty-tables")]
        skip_empty_tables: bool,

        /// Optional backup name
        backup_name: Option<String>,
    },

    /// Create a local backup and upload to S3 in one step
    #[command(name = "create_remote")]
    CreateRemote {
        /// Table filter pattern (globs: db.*, *.table)
        #[arg(short = 't', long = "tables")]
        tables: Option<String>,

        /// Remote incremental base backup name
        #[arg(long = "diff-from-remote")]
        diff_from_remote: Option<String>,

        /// Delete local backup after upload
        #[arg(long = "delete-source")]
        delete_source: bool,

        /// Include RBAC objects (users, roles, quotas, etc.)
        #[arg(long)]
        rbac: bool,

        /// Include ClickHouse server config files
        #[arg(long)]
        configs: bool,

        /// Include Named Collections
        #[arg(long = "named-collections")]
        named_collections: bool,

        /// Allow backup with inconsistent column types across parts
        #[arg(long = "skip-check-parts-columns")]
        skip_check_parts_columns: bool,

        /// Glob patterns for projections to skip
        #[arg(long = "skip-projections")]
        skip_projections: Option<String>,

        /// Resume interrupted operation from state file
        #[arg(long)]
        resume: bool,

        /// Optional backup name
        backup_name: Option<String>,
    },

    /// Download a remote backup and restore in one step
    #[command(name = "restore_remote")]
    RestoreRemote {
        /// Table filter pattern (globs: db.*, *.table)
        #[arg(short = 't', long = "tables")]
        tables: Option<String>,

        /// Rename single table: -t db.src --as=db.dst
        #[arg(long = "as", name = "rename")]
        rename_as: Option<String>,

        /// Bulk database remap: -m prod:staging,logs:logs_copy
        #[arg(short = 'm', long = "database-mapping")]
        database_mapping: Option<String>,

        /// DROP existing tables before restore
        #[arg(long = "rm", visible_alias = "drop")]
        rm: bool,

        /// Include RBAC objects (users, roles, quotas, etc.)
        #[arg(long)]
        rbac: bool,

        /// Include ClickHouse server config files
        #[arg(long)]
        configs: bool,

        /// Include Named Collections
        #[arg(long = "named-collections")]
        named_collections: bool,

        /// Skip restoring tables that have zero data parts
        #[arg(long = "skip-empty-tables")]
        skip_empty_tables: bool,

        /// Resume interrupted operation from state file
        #[arg(long)]
        resume: bool,

        /// Optional backup name
        backup_name: Option<String>,
    },

    /// List backups (local or remote)
    List {
        /// Show local or remote backups (default: both)
        #[arg(value_enum)]
        location: Option<Location>,

        /// Output format: default, json, yaml, csv, tsv
        #[arg(long, value_enum, default_value_t = ListFormat::Default)]
        format: ListFormat,
    },

    /// List tables from ClickHouse or from a remote backup
    Tables {
        /// Table filter pattern (globs: db.*, *.table)
        #[arg(short = 't', long = "tables")]
        tables: Option<String>,

        /// Show all tables including system
        #[arg(long)]
        all: bool,

        /// List tables from a remote backup instead of live ClickHouse
        #[arg(long = "remote-backup")]
        remote_backup: Option<String>,
    },

    /// Delete a backup
    Delete {
        /// Delete local or remote backup
        #[arg(value_enum)]
        location: Location,

        /// Backup name to delete
        backup_name: Option<String>,
    },

    /// Remove leftover shadow/ data
    Clean {
        /// Specific backup name to clean
        #[arg(long)]
        name: Option<String>,
    },

    /// Remove broken backups with missing/corrupt metadata
    #[command(name = "clean_broken")]
    CleanBroken {
        /// Clean local or remote broken backups
        #[arg(value_enum)]
        location: Location,
    },

    /// Print default config to stdout
    #[command(name = "default-config")]
    DefaultConfig,

    /// Print resolved config after env var overlay
    #[command(name = "print-config")]
    PrintConfig,

    /// Run scheduled backup watch loop
    Watch {
        /// Interval between watch checks (e.g. 1h, 30m)
        #[arg(long = "watch-interval")]
        watch_interval: Option<String>,

        /// Interval between full backups (e.g. 24h)
        #[arg(long = "full-interval")]
        full_interval: Option<String>,

        /// Backup name template
        #[arg(long = "name-template")]
        name_template: Option<String>,

        /// Table filter pattern (globs: db.*, *.table)
        #[arg(short = 't', long = "tables")]
        tables: Option<String>,
    },

    /// Start API server for Kubernetes
    Server {
        /// Enable watch loop alongside API server
        #[arg(long)]
        watch: bool,
    },
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn test_restore_schema_and_data_only_conflict() {
        // Passing both --schema and --data-only should be rejected by clap
        let result = Cli::try_parse_from([
            "chbackup",
            "restore",
            "--schema",
            "--data-only",
            "test-backup",
        ]);
        assert!(
            result.is_err(),
            "Expected error when both --schema and --data-only are passed"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("cannot be used with") || err.contains("conflict"),
            "Error should mention conflict: {}",
            err
        );
    }

    #[test]
    fn test_restore_schema_alone_ok() {
        let result = Cli::try_parse_from(["chbackup", "restore", "--schema", "test-backup"]);
        assert!(result.is_ok(), "Expected --schema alone to be accepted");
    }

    #[test]
    fn test_restore_data_only_alone_ok() {
        let result = Cli::try_parse_from(["chbackup", "restore", "--data-only", "test-backup"]);
        assert!(result.is_ok(), "Expected --data-only alone to be accepted");
    }

    #[test]
    fn test_create_has_no_resume_flag() {
        // --resume is intentionally absent from create; this test catches accidental re-addition
        let result = Cli::try_parse_from(["chbackup", "create", "--resume"]);
        assert!(
            result.is_err(),
            "create --resume should not be a valid flag"
        );
    }
}
