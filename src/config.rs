use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::{debug, warn};

/// Top-level configuration for chbackup.
/// Matches §12 of the design doc with ~106 params across 7 sections.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,

    #[serde(default)]
    pub clickhouse: ClickHouseConfig,

    #[serde(default)]
    pub s3: S3Config,

    #[serde(default)]
    pub backup: BackupConfig,

    #[serde(default)]
    pub retention: RetentionConfig,

    #[serde(default)]
    pub watch: WatchConfig,

    #[serde(default)]
    pub api: ApiConfig,
}

// ---------------------------------------------------------------------------
// GeneralConfig — 14 params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    /// debug | info | warning | error
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// text (human-readable) | json (structured, for Loki/ELK)
    #[serde(default = "default_log_format")]
    pub log_format: String,

    /// auto-disabled when stdout is not a TTY
    #[serde(default)]
    pub disable_progress_bar: bool,

    /// 0 = unlimited; -1 = delete local after successful upload
    #[serde(default)]
    pub backups_to_keep_local: i32,

    /// after upload, delete oldest exceeding count
    #[serde(default = "default_backups_to_keep_remote_general")]
    pub backups_to_keep_remote: i32,

    /// parallel part uploads (auto-tuned: round(sqrt(CPU/2)))
    #[serde(default = "default_concurrency_4")]
    pub upload_concurrency: u32,

    /// parallel part downloads
    #[serde(default = "default_concurrency_4")]
    pub download_concurrency: u32,

    /// 0 = no throttle; bytes/sec rate limit per part
    #[serde(default)]
    pub upload_max_bytes_per_second: u64,

    /// 0 = no throttle; bytes/sec rate limit per part
    #[serde(default)]
    pub download_max_bytes_per_second: u64,

    #[serde(default = "default_object_disk_server_side_copy_concurrency")]
    pub object_disk_server_side_copy_concurrency: u32,

    /// retry count for upload/download failures
    #[serde(default = "default_retries_on_failure_3")]
    pub retries_on_failure: u32,

    /// wait between retries
    #[serde(default = "default_retries_pause")]
    pub retries_pause: String,

    /// percent jitter on retries_pause (avoids thundering herd)
    #[serde(default = "default_retries_jitter_30")]
    pub retries_jitter: u32,

    /// track progress in state files for --resume
    #[serde(default = "default_true")]
    pub use_resumable_state: bool,

    /// TTL in seconds for the in-memory remote manifest cache (design 8.4).
    /// Used in server mode to avoid redundant S3 list calls. 0 = disabled.
    #[serde(default = "default_remote_cache_ttl_secs")]
    pub remote_cache_ttl_secs: u64,
}

// ---------------------------------------------------------------------------
// ClickHouseConfig — 37 params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickHouseConfig {
    #[serde(default = "default_localhost")]
    pub host: String,

    #[serde(default = "default_ch_port")]
    pub port: u16,

    #[serde(default = "default_ch_username")]
    pub username: String,

    #[serde(default)]
    pub password: String,

    #[serde(default = "default_data_path")]
    pub data_path: String,

    #[serde(default = "default_config_dir")]
    pub config_dir: String,

    /// use TLS for ClickHouse connection
    #[serde(default)]
    pub secure: bool,

    /// skip TLS certificate verification
    #[serde(default)]
    pub skip_verify: bool,

    /// TLS client key file
    #[serde(default)]
    pub tls_key: String,

    /// TLS client certificate file
    #[serde(default)]
    pub tls_cert: String,

    /// TLS custom CA file
    #[serde(default)]
    pub tls_ca: String,

    /// SYSTEM SYNC REPLICA before FREEZE
    #[serde(default = "default_true")]
    pub sync_replicated_tables: bool,

    /// wait for replication queue before ATTACH
    #[serde(default = "default_true")]
    pub check_replicas_before_attach: bool,

    /// validate column type consistency before backup
    #[serde(default)]
    pub check_parts_columns: bool,

    #[serde(default = "default_mutation_wait_timeout")]
    pub mutation_wait_timeout: String,

    /// use DETACH/ATTACH TABLE mode for full restores
    #[serde(default)]
    pub restore_as_attach: bool,

    /// execute DDL with ON CLUSTER clause
    #[serde(default)]
    pub restore_schema_on_cluster: String,

    /// rewrite Distributed engine cluster references during restore
    #[serde(default)]
    pub restore_distributed_cluster: String,

    /// concurrent restore table operations
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,

    /// log SQL queries at info level (false = debug level)
    #[serde(default = "default_true")]
    pub log_sql_queries: bool,

    /// skip tables dropped during backup (CH error 60/81)
    #[serde(default = "default_true")]
    pub ignore_not_exists_error_during_freeze: bool,

    /// freeze individual parts instead of whole table
    #[serde(default)]
    pub freeze_by_part: bool,

    /// WHERE clause for part filtering when freeze_by_part: true
    #[serde(default)]
    pub freeze_by_part_where: String,

    /// backup pending mutations from system.mutations
    #[serde(default = "default_true")]
    pub backup_mutations: bool,

    /// run after --rbac or --configs restore
    #[serde(default = "default_restart_command")]
    pub restart_command: String,

    /// verbose ClickHouse client debug logging
    #[serde(default)]
    pub debug: bool,

    /// always include RBAC objects in backup
    #[serde(default)]
    pub rbac_backup_always: bool,

    /// always include CH config files in backup
    #[serde(default)]
    pub config_backup_always: bool,

    /// always include named collections in backup
    #[serde(default)]
    pub named_collections_backup_always: bool,

    /// on RBAC restore conflict: "recreate", "ignore", "fail"
    #[serde(default = "default_rbac_resolve_conflicts")]
    pub rbac_resolve_conflicts: String,

    /// glob patterns to exclude
    #[serde(default = "default_skip_tables")]
    pub skip_tables: Vec<String>,

    /// engine names to exclude (e.g. ["Kafka", "S3Queue"])
    #[serde(default)]
    pub skip_table_engines: Vec<String>,

    /// disk names to exclude from backup
    #[serde(default)]
    pub skip_disks: Vec<String>,

    /// disk types to exclude (e.g. ["cache", "local"])
    #[serde(default)]
    pub skip_disk_types: Vec<String>,

    #[serde(default = "default_replica_path")]
    pub default_replica_path: String,

    #[serde(default = "default_replica_name")]
    pub default_replica_name: String,

    /// ClickHouse query timeout
    #[serde(default = "default_ch_timeout")]
    pub timeout: String,
}

// ---------------------------------------------------------------------------
// S3Config — 20 params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Config {
    #[serde(default = "default_s3_bucket")]
    pub bucket: String,

    #[serde(default = "default_s3_region")]
    pub region: String,

    /// for MinIO, R2, etc.
    #[serde(default)]
    pub endpoint: String,

    /// S3 key prefix. Supports {macro} expansion from system.macros
    #[serde(default = "default_s3_prefix")]
    pub prefix: String,

    #[serde(default)]
    pub access_key: String,

    #[serde(default)]
    pub secret_key: String,

    /// AWS IAM role to assume
    #[serde(default)]
    pub assume_role_arn: String,

    /// true for MinIO, Ceph
    #[serde(default)]
    pub force_path_style: bool,

    /// true for local S3-compatible stores
    #[serde(default)]
    pub disable_ssl: bool,

    /// skip S3 TLS certificate verification
    #[serde(default)]
    pub disable_cert_verification: bool,

    /// S3 ACL ("private", "bucket-owner-full-control", or "" for disabled)
    #[serde(default = "default_s3_acl")]
    pub acl: String,

    #[serde(default = "default_storage_class")]
    pub storage_class: String,

    /// AES256 | aws:kms
    #[serde(default)]
    pub sse: String,

    /// KMS key for aws:kms
    #[serde(default)]
    pub sse_kms_key_id: String,

    /// S3 multipart max parts
    #[serde(default = "default_max_parts_count")]
    pub max_parts_count: u32,

    /// S3 multipart chunk size (0 = auto: remote_size / max_parts_count)
    #[serde(default)]
    pub chunk_size: u64,

    /// S3 SDK internal concurrency per upload
    #[serde(default = "default_concurrency_1")]
    pub concurrency: u32,

    /// separate S3 prefix for object disk backup data
    #[serde(default)]
    pub object_disk_path: String,

    /// if CopyObject fails, fallback to streaming download+reupload
    #[serde(default)]
    pub allow_object_disk_streaming: bool,

    /// verbose S3 SDK request/response logging
    #[serde(default)]
    pub debug: bool,
}

// ---------------------------------------------------------------------------
// BackupConfig — 13 params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupConfig {
    #[serde(default = "default_backup_tables")]
    pub tables: String,

    /// true = create empty backup when no tables match filter
    #[serde(default)]
    pub allow_empty_backups: bool,

    /// lz4 | zstd | gzip | none
    #[serde(default = "default_compression")]
    pub compression: String,

    #[serde(default = "default_compression_level")]
    pub compression_level: u32,

    #[serde(default = "default_concurrency_4")]
    pub upload_concurrency: u32,

    #[serde(default = "default_concurrency_4")]
    pub download_concurrency: u32,

    #[serde(default = "default_object_disk_copy_concurrency")]
    pub object_disk_copy_concurrency: u32,

    /// 0 = unlimited
    #[serde(default)]
    pub upload_max_bytes_per_second: u64,

    /// 0 = unlimited
    #[serde(default)]
    pub download_max_bytes_per_second: u64,

    #[serde(default = "default_retries_on_failure_5")]
    pub retries_on_failure: u32,

    #[serde(default = "default_retries_duration")]
    pub retries_duration: String,

    /// randomize retry delay by +/-10%
    #[serde(default = "default_retries_jitter_01")]
    pub retries_jitter: f64,

    /// patterns like "db.table:proj_name"
    #[serde(default)]
    pub skip_projections: Vec<String>,

    /// Parts with uncompressed size above this threshold (in bytes) use
    /// streaming compression+multipart upload instead of buffering in memory.
    /// Default: 256 MiB.
    #[serde(default = "default_streaming_upload_threshold")]
    pub streaming_upload_threshold: u64,
}

// ---------------------------------------------------------------------------
// RetentionConfig — 2 params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// 0 = unlimited; -1 = delete local after upload
    #[serde(default)]
    pub backups_to_keep_local: i32,

    /// 0 = unlimited
    #[serde(default)]
    pub backups_to_keep_remote: i32,
}

// ---------------------------------------------------------------------------
// WatchConfig — 7 params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    /// enable watch loop in server mode
    #[serde(default)]
    pub enabled: bool,

    /// interval between incremental backups
    #[serde(default = "default_watch_interval")]
    pub watch_interval: String,

    /// interval between full backups
    #[serde(default = "default_full_interval")]
    pub full_interval: String,

    #[serde(default = "default_name_template")]
    pub name_template: String,

    /// table filter pattern for watch backups (None = match all)
    #[serde(default)]
    pub tables: Option<String>,

    /// abort after N consecutive failures
    #[serde(default = "default_max_consecutive_errors")]
    pub max_consecutive_errors: u32,

    /// wait before retrying after error
    #[serde(default = "default_retry_interval")]
    pub retry_interval: String,

    /// clean local backup after upload
    #[serde(default = "default_true")]
    pub delete_local_after_upload: bool,
}

// ---------------------------------------------------------------------------
// ApiConfig — 13 params
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    #[serde(default = "default_api_listen")]
    pub listen: String,

    #[serde(default = "default_true")]
    pub enable_metrics: bool,

    /// create system.backup_list and system.backup_actions URL tables
    #[serde(default = "default_true")]
    pub create_integration_tables: bool,

    /// DNS name for URL engine (default: localhost)
    #[serde(default)]
    pub integration_tables_host: String,

    /// basic auth username (empty = no auth)
    #[serde(default)]
    pub username: String,

    /// basic auth password
    #[serde(default)]
    pub password: String,

    /// use TLS for API endpoint
    #[serde(default)]
    pub secure: bool,

    /// TLS certificate file
    #[serde(default)]
    pub certificate_file: String,

    /// TLS private key file
    #[serde(default)]
    pub private_key_file: String,

    /// TLS CA cert file
    #[serde(default)]
    pub ca_cert_file: String,

    /// allow concurrent operations on different backup names
    #[serde(default)]
    pub allow_parallel: bool,

    /// auto-resume interrupted upload/download on server startup
    #[serde(default = "default_true")]
    pub complete_resumable_after_restart: bool,

    /// if watch loop dies unexpectedly, exit the server process
    #[serde(default)]
    pub watch_is_main_process: bool,
}

// ---------------------------------------------------------------------------
// Default implementations
// ---------------------------------------------------------------------------

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            log_level: default_log_level(),
            log_format: default_log_format(),
            disable_progress_bar: false,
            backups_to_keep_local: 0,
            backups_to_keep_remote: default_backups_to_keep_remote_general(),
            upload_concurrency: default_concurrency_4(),
            download_concurrency: default_concurrency_4(),
            upload_max_bytes_per_second: 0,
            download_max_bytes_per_second: 0,
            object_disk_server_side_copy_concurrency:
                default_object_disk_server_side_copy_concurrency(),
            retries_on_failure: default_retries_on_failure_3(),
            retries_pause: default_retries_pause(),
            retries_jitter: default_retries_jitter_30(),
            use_resumable_state: true,
            remote_cache_ttl_secs: default_remote_cache_ttl_secs(),
        }
    }
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            host: default_localhost(),
            port: default_ch_port(),
            username: default_ch_username(),
            password: String::new(),
            data_path: default_data_path(),
            config_dir: default_config_dir(),
            secure: false,
            skip_verify: false,
            tls_key: String::new(),
            tls_cert: String::new(),
            tls_ca: String::new(),
            sync_replicated_tables: true,
            check_replicas_before_attach: true,
            check_parts_columns: false,
            mutation_wait_timeout: default_mutation_wait_timeout(),
            restore_as_attach: false,
            restore_schema_on_cluster: String::new(),
            restore_distributed_cluster: String::new(),
            max_connections: default_max_connections(),
            log_sql_queries: true,
            ignore_not_exists_error_during_freeze: true,
            freeze_by_part: false,
            freeze_by_part_where: String::new(),
            backup_mutations: true,
            restart_command: default_restart_command(),
            debug: false,
            rbac_backup_always: false,
            config_backup_always: false,
            named_collections_backup_always: false,
            rbac_resolve_conflicts: default_rbac_resolve_conflicts(),
            skip_tables: default_skip_tables(),
            skip_table_engines: Vec::new(),
            skip_disks: Vec::new(),
            skip_disk_types: Vec::new(),
            default_replica_path: default_replica_path(),
            default_replica_name: default_replica_name(),
            timeout: default_ch_timeout(),
        }
    }
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            bucket: default_s3_bucket(),
            region: default_s3_region(),
            endpoint: String::new(),
            prefix: default_s3_prefix(),
            access_key: String::new(),
            secret_key: String::new(),
            assume_role_arn: String::new(),
            force_path_style: false,
            disable_ssl: false,
            disable_cert_verification: false,
            acl: default_s3_acl(),
            storage_class: default_storage_class(),
            sse: String::new(),
            sse_kms_key_id: String::new(),
            max_parts_count: default_max_parts_count(),
            chunk_size: 0,
            concurrency: default_concurrency_1(),
            object_disk_path: String::new(),
            allow_object_disk_streaming: false,
            debug: false,
        }
    }
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            tables: default_backup_tables(),
            allow_empty_backups: false,
            compression: default_compression(),
            compression_level: default_compression_level(),
            upload_concurrency: default_concurrency_4(),
            download_concurrency: default_concurrency_4(),
            object_disk_copy_concurrency: default_object_disk_copy_concurrency(),
            upload_max_bytes_per_second: 0,
            download_max_bytes_per_second: 0,
            retries_on_failure: default_retries_on_failure_5(),
            retries_duration: default_retries_duration(),
            retries_jitter: default_retries_jitter_01(),
            skip_projections: Vec::new(),
            streaming_upload_threshold: default_streaming_upload_threshold(),
        }
    }
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            watch_interval: default_watch_interval(),
            full_interval: default_full_interval(),
            name_template: default_name_template(),
            tables: None,
            max_consecutive_errors: default_max_consecutive_errors(),
            retry_interval: default_retry_interval(),
            delete_local_after_upload: true,
        }
    }
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            listen: default_api_listen(),
            enable_metrics: true,
            create_integration_tables: true,
            integration_tables_host: String::new(),
            username: String::new(),
            password: String::new(),
            secure: false,
            certificate_file: String::new(),
            private_key_file: String::new(),
            ca_cert_file: String::new(),
            allow_parallel: false,
            complete_resumable_after_restart: true,
            watch_is_main_process: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Default value helper functions
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "text".to_string()
}

fn default_backups_to_keep_remote_general() -> i32 {
    7
}

fn default_concurrency_4() -> u32 {
    4
}

fn default_concurrency_1() -> u32 {
    1
}

fn default_object_disk_server_side_copy_concurrency() -> u32 {
    32
}

fn default_retries_on_failure_3() -> u32 {
    3
}

fn default_retries_on_failure_5() -> u32 {
    5
}

/// 256 MiB default for streaming upload threshold.
fn default_streaming_upload_threshold() -> u64 {
    256 * 1024 * 1024
}

/// Default TTL for remote manifest cache: 300 seconds (5 minutes) per design 8.4.
fn default_remote_cache_ttl_secs() -> u64 {
    300
}

fn default_retries_pause() -> String {
    "5s".to_string()
}

fn default_retries_jitter_30() -> u32 {
    30
}

fn default_retries_jitter_01() -> f64 {
    0.1
}

fn default_localhost() -> String {
    "localhost".to_string()
}

fn default_ch_port() -> u16 {
    8123
}

fn default_ch_username() -> String {
    "default".to_string()
}

fn default_data_path() -> String {
    "/var/lib/clickhouse".to_string()
}

fn default_config_dir() -> String {
    "/etc/clickhouse-server".to_string()
}

fn default_mutation_wait_timeout() -> String {
    "5m".to_string()
}

fn default_max_connections() -> u32 {
    1
}

fn default_restart_command() -> String {
    "exec:systemctl restart clickhouse-server".to_string()
}

fn default_rbac_resolve_conflicts() -> String {
    "recreate".to_string()
}

fn default_skip_tables() -> Vec<String> {
    vec![
        "system.*".to_string(),
        "INFORMATION_SCHEMA.*".to_string(),
        "information_schema.*".to_string(),
        "_temporary_and_external_tables.*".to_string(),
    ]
}

fn default_replica_path() -> String {
    "/clickhouse/tables/{shard}/{database}/{table}".to_string()
}

fn default_replica_name() -> String {
    "{replica}".to_string()
}

fn default_ch_timeout() -> String {
    "5m".to_string()
}

fn default_s3_bucket() -> String {
    "my-backup-bucket".to_string()
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

fn default_s3_prefix() -> String {
    "chbackup".to_string()
}

fn default_s3_acl() -> String {
    String::new()
}

fn default_storage_class() -> String {
    "STANDARD".to_string()
}

fn default_max_parts_count() -> u32 {
    10000
}

fn default_backup_tables() -> String {
    "*.*".to_string()
}

fn default_compression() -> String {
    "lz4".to_string()
}

fn default_compression_level() -> u32 {
    1
}

fn default_object_disk_copy_concurrency() -> u32 {
    8
}

fn default_retries_duration() -> String {
    String::new()
}

fn default_watch_interval() -> String {
    "1h".to_string()
}

fn default_full_interval() -> String {
    "24h".to_string()
}

fn default_name_template() -> String {
    "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}".to_string()
}

fn default_max_consecutive_errors() -> u32 {
    5
}

fn default_retry_interval() -> String {
    "5m".to_string()
}

fn default_api_listen() -> String {
    "localhost:7171".to_string()
}

// ---------------------------------------------------------------------------
// Config loading and env overlay
// ---------------------------------------------------------------------------

impl Config {
    /// Load config from YAML file at the given path. If the file does not exist,
    /// returns default config. After loading, applies environment variable overlay
    /// and CLI --env overrides.
    pub fn load(path: &Path, cli_env_overrides: &[String]) -> Result<Self> {
        let mut config = if path.exists() {
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {}", path.display()))?;
            serde_yaml::from_str(&contents)
                .with_context(|| format!("Failed to parse config file: {}", path.display()))?
        } else {
            Config::default()
        };

        // Apply environment variable overlay
        config.apply_env_overlay();

        // Apply CLI --env overrides (these take priority over env vars)
        config.apply_cli_env_overrides(cli_env_overrides)?;

        // Validate the final config
        config.validate()?;

        Ok(config)
    }

    /// Serialize the default config to a YAML string.
    pub fn default_yaml() -> Result<String> {
        let config = Config::default();
        serde_yaml::to_string(&config).context("Failed to serialize default config to YAML")
    }

    /// Apply environment variable overlay. Maps well-known env vars to config fields.
    /// Every config parameter can be overridden via an environment variable (design doc §2).
    fn apply_env_overlay(&mut self) {
        // General
        if let Ok(v) = std::env::var("CHBACKUP_LOG_LEVEL") {
            self.general.log_level = v;
        }
        if let Ok(v) = std::env::var("CHBACKUP_LOG_FORMAT") {
            self.general.log_format = v;
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUPS_TO_KEEP_LOCAL") {
            if let Ok(n) = v.parse::<i32>() {
                self.general.backups_to_keep_local = n;
            } else {
                warn!("CHBACKUP_BACKUPS_TO_KEEP_LOCAL='{}' is not a valid i32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUPS_TO_KEEP_REMOTE") {
            if let Ok(n) = v.parse::<i32>() {
                self.general.backups_to_keep_remote = n;
            } else {
                warn!("CHBACKUP_BACKUPS_TO_KEEP_REMOTE='{}' is not a valid i32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_UPLOAD_CONCURRENCY") {
            if let Ok(n) = v.parse::<u32>() {
                self.general.upload_concurrency = n;
            } else {
                warn!("CHBACKUP_UPLOAD_CONCURRENCY='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_DOWNLOAD_CONCURRENCY") {
            if let Ok(n) = v.parse::<u32>() {
                self.general.download_concurrency = n;
            } else {
                warn!("CHBACKUP_DOWNLOAD_CONCURRENCY='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_RETRIES_ON_FAILURE") {
            if let Ok(n) = v.parse::<u32>() {
                self.general.retries_on_failure = n;
            } else {
                warn!("CHBACKUP_RETRIES_ON_FAILURE='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_RETRIES_PAUSE") {
            self.general.retries_pause = v;
        }
        if let Ok(v) = std::env::var("CHBACKUP_REMOTE_CACHE_TTL_SECS") {
            if let Ok(n) = v.parse::<u64>() {
                self.general.remote_cache_ttl_secs = n;
            } else {
                warn!("CHBACKUP_REMOTE_CACHE_TTL_SECS='{}' is not a valid u64, ignoring", v);
            }
        }

        // ClickHouse
        if let Ok(v) = std::env::var("CLICKHOUSE_HOST") {
            self.clickhouse.host = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_PORT") {
            if let Ok(port) = v.parse::<u16>() {
                self.clickhouse.port = port;
            } else {
                warn!("CLICKHOUSE_PORT='{}' is not a valid u16, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_USERNAME") {
            self.clickhouse.username = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_PASSWORD") {
            self.clickhouse.password = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_DATA_PATH") {
            self.clickhouse.data_path = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_SECURE") {
            if let Ok(b) = v.parse::<bool>() {
                self.clickhouse.secure = b;
            } else {
                warn!("CLICKHOUSE_SECURE='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_SKIP_VERIFY") {
            if let Ok(b) = v.parse::<bool>() {
                self.clickhouse.skip_verify = b;
            } else {
                warn!("CLICKHOUSE_SKIP_VERIFY='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_TLS_KEY") {
            self.clickhouse.tls_key = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_TLS_CERT") {
            self.clickhouse.tls_cert = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_TLS_CA") {
            self.clickhouse.tls_ca = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_SYNC_REPLICATED_TABLES") {
            if let Ok(b) = v.parse::<bool>() {
                self.clickhouse.sync_replicated_tables = b;
            } else {
                warn!("CLICKHOUSE_SYNC_REPLICATED_TABLES='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_MAX_CONNECTIONS") {
            if let Ok(n) = v.parse::<u32>() {
                self.clickhouse.max_connections = n;
            } else {
                warn!("CLICKHOUSE_MAX_CONNECTIONS='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_TIMEOUT") {
            self.clickhouse.timeout = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_CONFIG_DIR") {
            self.clickhouse.config_dir = v;
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_DEBUG") {
            if let Ok(b) = v.parse::<bool>() {
                self.clickhouse.debug = b;
            } else {
                warn!("CLICKHOUSE_DEBUG='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_SKIP_TABLE_ENGINES") {
            self.clickhouse.skip_table_engines = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_SKIP_DISKS") {
            self.clickhouse.skip_disks = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_SKIP_DISK_TYPES") {
            self.clickhouse.skip_disk_types = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = std::env::var("CLICKHOUSE_SKIP_TABLES") {
            self.clickhouse.skip_tables = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }

        // S3
        if let Ok(v) = std::env::var("S3_BUCKET") {
            self.s3.bucket = v;
        }
        if let Ok(v) = std::env::var("S3_REGION") {
            self.s3.region = v;
        }
        if let Ok(v) = std::env::var("S3_ENDPOINT") {
            self.s3.endpoint = v;
        }
        if let Ok(v) = std::env::var("S3_PREFIX") {
            self.s3.prefix = v;
        }
        if let Ok(v) = std::env::var("S3_ACCESS_KEY") {
            self.s3.access_key = v;
        }
        if let Ok(v) = std::env::var("S3_SECRET_KEY") {
            self.s3.secret_key = v;
        }
        if let Ok(v) = std::env::var("S3_ASSUME_ROLE_ARN") {
            self.s3.assume_role_arn = v;
        }
        if let Ok(v) = std::env::var("S3_FORCE_PATH_STYLE") {
            if let Ok(b) = v.parse::<bool>() {
                self.s3.force_path_style = b;
            } else {
                warn!("S3_FORCE_PATH_STYLE='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("S3_ACL") {
            self.s3.acl = v;
        }
        if let Ok(v) = std::env::var("S3_STORAGE_CLASS") {
            self.s3.storage_class = v;
        }
        if let Ok(v) = std::env::var("S3_SSE") {
            self.s3.sse = v;
        }
        if let Ok(v) = std::env::var("S3_SSE_KMS_KEY_ID") {
            self.s3.sse_kms_key_id = v;
        }
        if let Ok(v) = std::env::var("S3_DISABLE_SSL") {
            if let Ok(b) = v.parse::<bool>() {
                self.s3.disable_ssl = b;
            } else {
                warn!("S3_DISABLE_SSL='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("S3_DISABLE_CERT_VERIFICATION") {
            if let Ok(b) = v.parse::<bool>() {
                self.s3.disable_cert_verification = b;
            } else {
                warn!("S3_DISABLE_CERT_VERIFICATION='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("S3_CONCURRENCY") {
            if let Ok(n) = v.parse::<u32>() {
                self.s3.concurrency = n;
            } else {
                warn!("S3_CONCURRENCY='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("S3_OBJECT_DISK_PATH") {
            self.s3.object_disk_path = v;
        }

        // Backup
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_COMPRESSION") {
            self.backup.compression = v;
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_UPLOAD_CONCURRENCY") {
            if let Ok(n) = v.parse::<u32>() {
                self.backup.upload_concurrency = n;
            } else {
                warn!("CHBACKUP_BACKUP_UPLOAD_CONCURRENCY='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY") {
            if let Ok(n) = v.parse::<u32>() {
                self.backup.download_concurrency = n;
            } else {
                warn!("CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_RETRIES_ON_FAILURE") {
            if let Ok(n) = v.parse::<u32>() {
                self.backup.retries_on_failure = n;
            } else {
                warn!("CHBACKUP_BACKUP_RETRIES_ON_FAILURE='{}' is not a valid u32, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_RETRIES_DURATION") {
            self.backup.retries_duration = v;
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_TABLES") {
            self.backup.tables = v;
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_SKIP_PROJECTIONS") {
            self.backup.skip_projections = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Ok(v) = std::env::var("CHBACKUP_BACKUP_STREAMING_UPLOAD_THRESHOLD") {
            if let Ok(n) = v.parse::<u64>() {
                self.backup.streaming_upload_threshold = n;
            } else {
                warn!("CHBACKUP_BACKUP_STREAMING_UPLOAD_THRESHOLD='{}' is not a valid u64, ignoring", v);
            }
        }

        // API
        if let Ok(v) = std::env::var("API_LISTEN") {
            self.api.listen = v;
        }
        if let Ok(v) = std::env::var("API_SECURE") {
            if let Ok(b) = v.parse::<bool>() {
                self.api.secure = b;
            } else {
                warn!("API_SECURE='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("API_USERNAME") {
            self.api.username = v;
        }
        if let Ok(v) = std::env::var("API_PASSWORD") {
            self.api.password = v;
        }
        if let Ok(v) = std::env::var("API_CREATE_INTEGRATION_TABLES") {
            if let Ok(b) = v.parse::<bool>() {
                self.api.create_integration_tables = b;
            } else {
                warn!("API_CREATE_INTEGRATION_TABLES='{}' is not a valid bool, ignoring", v);
            }
        }

        // Watch
        if let Ok(v) = std::env::var("WATCH_INTERVAL") {
            self.watch.watch_interval = v;
        }
        if let Ok(v) = std::env::var("FULL_INTERVAL") {
            self.watch.full_interval = v;
        }
        if let Ok(v) = std::env::var("WATCH_ENABLED") {
            if let Ok(b) = v.parse::<bool>() {
                self.watch.enabled = b;
            } else {
                warn!("WATCH_ENABLED='{}' is not a valid bool, ignoring", v);
            }
        }
        if let Ok(v) = std::env::var("WATCH_MAX_CONSECUTIVE_ERRORS") {
            if let Ok(n) = v.parse::<u32>() {
                self.watch.max_consecutive_errors = n;
            } else {
                warn!("WATCH_MAX_CONSECUTIVE_ERRORS='{}' is not a valid u32, ignoring", v);
            }
        }
    }

    /// Apply CLI --env KEY=VALUE overrides. These take priority over env vars.
    /// Supported keys use dot notation (s3.bucket, clickhouse.host) or
    /// env-style keys (S3_BUCKET, CLICKHOUSE_HOST). Env-style keys are
    /// translated to dot notation via `env_key_to_dot_notation()`.
    fn apply_cli_env_overrides(&mut self, overrides: &[String]) -> Result<()> {
        for kv in overrides {
            let (key, value) = kv.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("Invalid --env format: '{}'. Expected KEY=VALUE", kv)
            })?;

            let trimmed_key = key.trim();
            let effective_key = if let Some(dot_key) = env_key_to_dot_notation(trimmed_key) {
                debug!(
                    env_key = %trimmed_key,
                    dot_key = %dot_key,
                    "Translated env-style key to dot notation"
                );
                dot_key
            } else {
                trimmed_key
            };

            self.set_field(effective_key, value.trim())?;
        }
        Ok(())
    }

    /// Set a config field by dot-notation key (e.g., "s3.bucket", "clickhouse.port").
    fn set_field(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            // General
            "general.log_level" => self.general.log_level = value.to_string(),
            "general.log_format" => self.general.log_format = value.to_string(),
            "general.disable_progress_bar" => {
                self.general.disable_progress_bar = value
                    .parse()
                    .context("Invalid bool for general.disable_progress_bar")?
            }
            "general.backups_to_keep_local" => {
                self.general.backups_to_keep_local = value
                    .parse()
                    .context("Invalid i32 for general.backups_to_keep_local")?
            }
            "general.backups_to_keep_remote" => {
                self.general.backups_to_keep_remote = value
                    .parse()
                    .context("Invalid i32 for general.backups_to_keep_remote")?
            }
            "general.upload_concurrency" => {
                self.general.upload_concurrency = value
                    .parse()
                    .context("Invalid u32 for general.upload_concurrency")?
            }
            "general.download_concurrency" => {
                self.general.download_concurrency = value
                    .parse()
                    .context("Invalid u32 for general.download_concurrency")?
            }
            "general.upload_max_bytes_per_second" => {
                self.general.upload_max_bytes_per_second = value.parse().context("Invalid u64")?
            }
            "general.download_max_bytes_per_second" => {
                self.general.download_max_bytes_per_second = value.parse().context("Invalid u64")?
            }
            "general.object_disk_server_side_copy_concurrency" => {
                self.general.object_disk_server_side_copy_concurrency =
                    value.parse().context("Invalid u32")?
            }
            "general.retries_on_failure" => {
                self.general.retries_on_failure = value.parse().context("Invalid u32")?
            }
            "general.retries_pause" => self.general.retries_pause = value.to_string(),
            "general.retries_jitter" => {
                self.general.retries_jitter = value.parse().context("Invalid u32")?
            }
            "general.use_resumable_state" => {
                self.general.use_resumable_state = value.parse().context("Invalid bool")?
            }
            "general.remote_cache_ttl_secs" => {
                self.general.remote_cache_ttl_secs = value.parse().context("Invalid u64")?
            }

            // ClickHouse
            "clickhouse.host" => self.clickhouse.host = value.to_string(),
            "clickhouse.port" => {
                self.clickhouse.port = value.parse().context("Invalid u16 for clickhouse.port")?
            }
            "clickhouse.username" => self.clickhouse.username = value.to_string(),
            "clickhouse.password" => self.clickhouse.password = value.to_string(),
            "clickhouse.data_path" => self.clickhouse.data_path = value.to_string(),
            "clickhouse.config_dir" => self.clickhouse.config_dir = value.to_string(),
            "clickhouse.secure" => {
                self.clickhouse.secure = value.parse().context("Invalid bool")?
            }
            "clickhouse.skip_verify" => {
                self.clickhouse.skip_verify = value.parse().context("Invalid bool")?
            }
            "clickhouse.tls_key" => self.clickhouse.tls_key = value.to_string(),
            "clickhouse.tls_cert" => self.clickhouse.tls_cert = value.to_string(),
            "clickhouse.tls_ca" => self.clickhouse.tls_ca = value.to_string(),
            "clickhouse.sync_replicated_tables" => {
                self.clickhouse.sync_replicated_tables = value.parse().context("Invalid bool")?
            }
            "clickhouse.check_replicas_before_attach" => {
                self.clickhouse.check_replicas_before_attach =
                    value.parse().context("Invalid bool")?
            }
            "clickhouse.check_parts_columns" => {
                self.clickhouse.check_parts_columns = value.parse().context("Invalid bool")?
            }
            "clickhouse.mutation_wait_timeout" => {
                self.clickhouse.mutation_wait_timeout = value.to_string()
            }
            "clickhouse.restore_as_attach" => {
                self.clickhouse.restore_as_attach = value.parse().context("Invalid bool")?
            }
            "clickhouse.restore_schema_on_cluster" => {
                self.clickhouse.restore_schema_on_cluster = value.to_string()
            }
            "clickhouse.restore_distributed_cluster" => {
                self.clickhouse.restore_distributed_cluster = value.to_string()
            }
            "clickhouse.max_connections" => {
                self.clickhouse.max_connections = value.parse().context("Invalid u32")?
            }
            "clickhouse.log_sql_queries" => {
                self.clickhouse.log_sql_queries = value.parse().context("Invalid bool")?
            }
            "clickhouse.ignore_not_exists_error_during_freeze" => {
                self.clickhouse.ignore_not_exists_error_during_freeze =
                    value.parse().context("Invalid bool")?
            }
            "clickhouse.freeze_by_part" => {
                self.clickhouse.freeze_by_part = value.parse().context("Invalid bool")?
            }
            "clickhouse.freeze_by_part_where" => {
                self.clickhouse.freeze_by_part_where = value.to_string()
            }
            "clickhouse.backup_mutations" => {
                self.clickhouse.backup_mutations = value.parse().context("Invalid bool")?
            }
            "clickhouse.restart_command" => self.clickhouse.restart_command = value.to_string(),
            "clickhouse.debug" => self.clickhouse.debug = value.parse().context("Invalid bool")?,
            "clickhouse.rbac_backup_always" => {
                self.clickhouse.rbac_backup_always = value.parse().context("Invalid bool")?
            }
            "clickhouse.config_backup_always" => {
                self.clickhouse.config_backup_always = value.parse().context("Invalid bool")?
            }
            "clickhouse.named_collections_backup_always" => {
                self.clickhouse.named_collections_backup_always =
                    value.parse().context("Invalid bool")?
            }
            "clickhouse.rbac_resolve_conflicts" => {
                self.clickhouse.rbac_resolve_conflicts = value.to_string()
            }
            "clickhouse.default_replica_path" => {
                self.clickhouse.default_replica_path = value.to_string()
            }
            "clickhouse.default_replica_name" => {
                self.clickhouse.default_replica_name = value.to_string()
            }
            "clickhouse.timeout" => self.clickhouse.timeout = value.to_string(),
            "clickhouse.skip_disks" => {
                self.clickhouse.skip_disks = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "clickhouse.skip_disk_types" => {
                self.clickhouse.skip_disk_types = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "clickhouse.skip_table_engines" => {
                self.clickhouse.skip_table_engines = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "clickhouse.skip_tables" => {
                self.clickhouse.skip_tables = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }

            // S3
            "s3.bucket" => self.s3.bucket = value.to_string(),
            "s3.region" => self.s3.region = value.to_string(),
            "s3.endpoint" => self.s3.endpoint = value.to_string(),
            "s3.prefix" => self.s3.prefix = value.to_string(),
            "s3.access_key" => self.s3.access_key = value.to_string(),
            "s3.secret_key" => self.s3.secret_key = value.to_string(),
            "s3.assume_role_arn" => self.s3.assume_role_arn = value.to_string(),
            "s3.force_path_style" => {
                self.s3.force_path_style = value.parse().context("Invalid bool")?
            }
            "s3.disable_ssl" => self.s3.disable_ssl = value.parse().context("Invalid bool")?,
            "s3.disable_cert_verification" => {
                self.s3.disable_cert_verification = value.parse().context("Invalid bool")?
            }
            "s3.acl" => self.s3.acl = value.to_string(),
            "s3.storage_class" => self.s3.storage_class = value.to_string(),
            "s3.sse" => self.s3.sse = value.to_string(),
            "s3.sse_kms_key_id" => self.s3.sse_kms_key_id = value.to_string(),
            "s3.max_parts_count" => {
                self.s3.max_parts_count = value.parse().context("Invalid u32")?
            }
            "s3.chunk_size" => self.s3.chunk_size = value.parse().context("Invalid u64")?,
            "s3.concurrency" => self.s3.concurrency = value.parse().context("Invalid u32")?,
            "s3.object_disk_path" => self.s3.object_disk_path = value.to_string(),
            "s3.allow_object_disk_streaming" => {
                self.s3.allow_object_disk_streaming = value.parse().context("Invalid bool")?
            }
            "s3.debug" => self.s3.debug = value.parse().context("Invalid bool")?,

            // Backup
            "backup.tables" => self.backup.tables = value.to_string(),
            "backup.allow_empty_backups" => {
                self.backup.allow_empty_backups = value.parse().context("Invalid bool")?
            }
            "backup.compression" => self.backup.compression = value.to_string(),
            "backup.compression_level" => {
                self.backup.compression_level = value.parse().context("Invalid u32")?
            }
            "backup.upload_concurrency" => {
                self.backup.upload_concurrency = value.parse().context("Invalid u32")?
            }
            "backup.download_concurrency" => {
                self.backup.download_concurrency = value.parse().context("Invalid u32")?
            }
            "backup.object_disk_copy_concurrency" => {
                self.backup.object_disk_copy_concurrency = value.parse().context("Invalid u32")?
            }
            "backup.upload_max_bytes_per_second" => {
                self.backup.upload_max_bytes_per_second = value.parse().context("Invalid u64")?
            }
            "backup.download_max_bytes_per_second" => {
                self.backup.download_max_bytes_per_second = value.parse().context("Invalid u64")?
            }
            "backup.retries_on_failure" => {
                self.backup.retries_on_failure = value.parse().context("Invalid u32")?
            }
            "backup.retries_duration" => self.backup.retries_duration = value.to_string(),
            "backup.retries_jitter" => {
                self.backup.retries_jitter = value.parse().context("Invalid f64")?
            }
            "backup.skip_projections" => {
                self.backup.skip_projections = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "backup.streaming_upload_threshold" => {
                self.backup.streaming_upload_threshold = value
                    .parse()
                    .context("Invalid u64 for backup.streaming_upload_threshold")?
            }

            // Retention
            "retention.backups_to_keep_local" => {
                self.retention.backups_to_keep_local = value.parse().context("Invalid i32")?
            }
            "retention.backups_to_keep_remote" => {
                self.retention.backups_to_keep_remote = value.parse().context("Invalid i32")?
            }

            // Watch
            "watch.enabled" => self.watch.enabled = value.parse().context("Invalid bool")?,
            "watch.watch_interval" => self.watch.watch_interval = value.to_string(),
            "watch.full_interval" => self.watch.full_interval = value.to_string(),
            "watch.name_template" => self.watch.name_template = value.to_string(),
            "watch.tables" => self.watch.tables = Some(value.to_string()),
            "watch.max_consecutive_errors" => {
                self.watch.max_consecutive_errors = value.parse().context("Invalid u32")?
            }
            "watch.retry_interval" => self.watch.retry_interval = value.to_string(),
            "watch.delete_local_after_upload" => {
                self.watch.delete_local_after_upload = value.parse().context("Invalid bool")?
            }

            // API
            "api.listen" => self.api.listen = value.to_string(),
            "api.enable_metrics" => {
                self.api.enable_metrics = value.parse().context("Invalid bool")?
            }
            "api.create_integration_tables" => {
                self.api.create_integration_tables = value.parse().context("Invalid bool")?
            }
            "api.integration_tables_host" => self.api.integration_tables_host = value.to_string(),
            "api.username" => self.api.username = value.to_string(),
            "api.password" => self.api.password = value.to_string(),
            "api.secure" => self.api.secure = value.parse().context("Invalid bool")?,
            "api.certificate_file" => self.api.certificate_file = value.to_string(),
            "api.private_key_file" => self.api.private_key_file = value.to_string(),
            "api.ca_cert_file" => self.api.ca_cert_file = value.to_string(),
            "api.allow_parallel" => {
                self.api.allow_parallel = value.parse().context("Invalid bool")?
            }
            "api.complete_resumable_after_restart" => {
                self.api.complete_resumable_after_restart = value.parse().context("Invalid bool")?
            }
            "api.watch_is_main_process" => {
                self.api.watch_is_main_process = value.parse().context("Invalid bool")?
            }

            _ => {
                return Err(anyhow::anyhow!("Unknown config key: '{}'", key));
            }
        }
        Ok(())
    }

    /// Validate the loaded config.
    pub fn validate(&self) -> Result<()> {
        // Concurrency checks
        if self.general.upload_concurrency == 0 {
            return Err(anyhow::anyhow!("general.upload_concurrency must be > 0"));
        }
        if self.general.download_concurrency == 0 {
            return Err(anyhow::anyhow!("general.download_concurrency must be > 0"));
        }
        if self.s3.concurrency == 0 {
            return Err(anyhow::anyhow!("s3.concurrency must be > 0"));
        }
        if self.clickhouse.max_connections == 0 {
            return Err(anyhow::anyhow!("clickhouse.max_connections must be > 0"));
        }
        if self.backup.object_disk_copy_concurrency == 0 {
            return Err(anyhow::anyhow!(
                "backup.object_disk_copy_concurrency must be > 0"
            ));
        }
        if self.general.object_disk_server_side_copy_concurrency == 0 {
            return Err(anyhow::anyhow!(
                "general.object_disk_server_side_copy_concurrency must be > 0"
            ));
        }
        if self.s3.max_parts_count == 0 {
            return Err(anyhow::anyhow!("s3.max_parts_count must be > 0"));
        }

        // Watch interval validation: full_interval must be greater than watch_interval.
        // Always validate regardless of watch.enabled -- watch can be started via CLI
        // --watch flag, `chbackup watch` command, or API without setting watch.enabled=true.
        {
            let watch_secs = parse_duration_secs(&self.watch.watch_interval)
                .context("Invalid watch.watch_interval duration")?;
            let full_secs = parse_duration_secs(&self.watch.full_interval)
                .context("Invalid watch.full_interval duration")?;
            if full_secs <= watch_secs {
                return Err(anyhow::anyhow!(
                    "watch.full_interval ({}) must be greater than watch.watch_interval ({})",
                    self.watch.full_interval,
                    self.watch.watch_interval
                ));
            }
        }

        // Validate log level
        match self.general.log_level.as_str() {
            "debug" | "info" | "warning" | "warn" | "error" | "trace" => {}
            other => {
                return Err(anyhow::anyhow!(
                    "Invalid general.log_level: '{}'. Expected: debug, info, warning, error",
                    other
                ));
            }
        }

        // Validate log format
        match self.general.log_format.as_str() {
            "text" | "json" => {}
            other => {
                return Err(anyhow::anyhow!(
                    "Invalid general.log_format: '{}'. Expected: text, json",
                    other
                ));
            }
        }

        // Validate compression
        match self.backup.compression.as_str() {
            "lz4" | "zstd" | "gzip" | "none" => {}
            other => {
                return Err(anyhow::anyhow!(
                    "Invalid backup.compression: '{}'. Expected: lz4, zstd, gzip, none",
                    other
                ));
            }
        }

        // Validate compression_level bounds per format
        if self.backup.compression == "zstd" && self.backup.compression_level > 22 {
            return Err(anyhow::anyhow!(
                "zstd compression_level must be 0-22, got {}",
                self.backup.compression_level
            ));
        }
        if self.backup.compression == "gzip" && self.backup.compression_level > 9 {
            return Err(anyhow::anyhow!(
                "gzip compression_level must be 0-9, got {}",
                self.backup.compression_level
            ));
        }

        // Validate rbac_resolve_conflicts
        match self.clickhouse.rbac_resolve_conflicts.as_str() {
            "recreate" | "ignore" | "fail" => {}
            other => {
                return Err(anyhow::anyhow!(
                    "Invalid clickhouse.rbac_resolve_conflicts: '{}'. Expected: recreate, ignore, fail",
                    other
                ));
            }
        }

        // Validate API auth: username and password must both be set or both be empty
        let user_set = !self.api.username.is_empty();
        let pass_set = !self.api.password.is_empty();
        if user_set != pass_set {
            return Err(anyhow::anyhow!(
                "api.username and api.password must both be set or both be empty"
            ));
        }

        Ok(())
    }
}

/// Translate an env-style key (e.g. `S3_BUCKET`, `CLICKHOUSE_HOST`) to its
/// dot-notation equivalent (e.g. `s3.bucket`, `clickhouse.host`).
///
/// Returns `Some(dot_key)` for known env var names from `apply_env_overlay()`,
/// or `None` if the key is not recognized (allowing dot-notation passthrough).
fn env_key_to_dot_notation(key: &str) -> Option<&'static str> {
    match key {
        // General
        "CHBACKUP_LOG_LEVEL" => Some("general.log_level"),
        "CHBACKUP_LOG_FORMAT" => Some("general.log_format"),
        "CHBACKUP_BACKUPS_TO_KEEP_LOCAL" => Some("general.backups_to_keep_local"),
        "CHBACKUP_BACKUPS_TO_KEEP_REMOTE" => Some("general.backups_to_keep_remote"),
        "CHBACKUP_UPLOAD_CONCURRENCY" => Some("general.upload_concurrency"),
        "CHBACKUP_DOWNLOAD_CONCURRENCY" => Some("general.download_concurrency"),
        "CHBACKUP_RETRIES_ON_FAILURE" => Some("general.retries_on_failure"),
        "CHBACKUP_RETRIES_PAUSE" => Some("general.retries_pause"),
        "CHBACKUP_REMOTE_CACHE_TTL_SECS" => Some("general.remote_cache_ttl_secs"),

        // ClickHouse
        "CLICKHOUSE_HOST" => Some("clickhouse.host"),
        "CLICKHOUSE_PORT" => Some("clickhouse.port"),
        "CLICKHOUSE_USERNAME" => Some("clickhouse.username"),
        "CLICKHOUSE_PASSWORD" => Some("clickhouse.password"),
        "CLICKHOUSE_DATA_PATH" => Some("clickhouse.data_path"),
        "CLICKHOUSE_SECURE" => Some("clickhouse.secure"),
        "CLICKHOUSE_SKIP_VERIFY" => Some("clickhouse.skip_verify"),
        "CLICKHOUSE_TLS_KEY" => Some("clickhouse.tls_key"),
        "CLICKHOUSE_TLS_CERT" => Some("clickhouse.tls_cert"),
        "CLICKHOUSE_TLS_CA" => Some("clickhouse.tls_ca"),
        "CLICKHOUSE_SYNC_REPLICATED_TABLES" => Some("clickhouse.sync_replicated_tables"),
        "CLICKHOUSE_MAX_CONNECTIONS" => Some("clickhouse.max_connections"),
        "CLICKHOUSE_TIMEOUT" => Some("clickhouse.timeout"),
        "CLICKHOUSE_CONFIG_DIR" => Some("clickhouse.config_dir"),
        "CLICKHOUSE_DEBUG" => Some("clickhouse.debug"),
        "CLICKHOUSE_SKIP_DISKS" => Some("clickhouse.skip_disks"),
        "CLICKHOUSE_SKIP_DISK_TYPES" => Some("clickhouse.skip_disk_types"),
        "CLICKHOUSE_SKIP_TABLE_ENGINES" => Some("clickhouse.skip_table_engines"),
        "CLICKHOUSE_SKIP_TABLES" => Some("clickhouse.skip_tables"),

        // S3
        "S3_BUCKET" => Some("s3.bucket"),
        "S3_REGION" => Some("s3.region"),
        "S3_ENDPOINT" => Some("s3.endpoint"),
        "S3_PREFIX" => Some("s3.prefix"),
        "S3_ACCESS_KEY" => Some("s3.access_key"),
        "S3_SECRET_KEY" => Some("s3.secret_key"),
        "S3_ASSUME_ROLE_ARN" => Some("s3.assume_role_arn"),
        "S3_FORCE_PATH_STYLE" => Some("s3.force_path_style"),
        "S3_ACL" => Some("s3.acl"),
        "S3_STORAGE_CLASS" => Some("s3.storage_class"),
        "S3_SSE" => Some("s3.sse"),
        "S3_SSE_KMS_KEY_ID" => Some("s3.sse_kms_key_id"),
        "S3_DISABLE_SSL" => Some("s3.disable_ssl"),
        "S3_DISABLE_CERT_VERIFICATION" => Some("s3.disable_cert_verification"),
        "S3_CONCURRENCY" => Some("s3.concurrency"),
        "S3_OBJECT_DISK_PATH" => Some("s3.object_disk_path"),

        // Backup
        "CHBACKUP_BACKUP_COMPRESSION" => Some("backup.compression"),
        "CHBACKUP_BACKUP_UPLOAD_CONCURRENCY" => Some("backup.upload_concurrency"),
        "CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY" => Some("backup.download_concurrency"),
        "CHBACKUP_BACKUP_RETRIES_ON_FAILURE" => Some("backup.retries_on_failure"),
        "CHBACKUP_BACKUP_RETRIES_DURATION" => Some("backup.retries_duration"),
        "CHBACKUP_BACKUP_TABLES" => Some("backup.tables"),
        "CHBACKUP_BACKUP_SKIP_PROJECTIONS" => Some("backup.skip_projections"),
        "CHBACKUP_BACKUP_STREAMING_UPLOAD_THRESHOLD" => Some("backup.streaming_upload_threshold"),

        // API
        "API_LISTEN" => Some("api.listen"),
        "API_SECURE" => Some("api.secure"),
        "API_USERNAME" => Some("api.username"),
        "API_PASSWORD" => Some("api.password"),
        "API_CREATE_INTEGRATION_TABLES" => Some("api.create_integration_tables"),

        // Watch
        "WATCH_INTERVAL" => Some("watch.watch_interval"),
        "FULL_INTERVAL" => Some("watch.full_interval"),
        "WATCH_ENABLED" => Some("watch.enabled"),
        "WATCH_MAX_CONSECUTIVE_ERRORS" => Some("watch.max_consecutive_errors"),

        _ => None,
    }
}

/// Parse a simple duration string (e.g. "1h", "24h", "30m", "5s", "10s") into seconds.
/// Supports h (hours), m (minutes), s (seconds) suffixes.
pub fn parse_duration_secs(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        return Err(anyhow::anyhow!("Empty duration string"));
    }

    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('h') {
        (n, 3600u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60u64)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1u64)
    } else {
        // Try parsing as plain seconds
        (s, 1u64)
    };

    let num: u64 = num_str
        .parse()
        .with_context(|| format!("Invalid duration number: '{}'", num_str))?;

    Ok(num.saturating_mul(multiplier))
}

/// Resolve effective retry configuration.
///
/// Returns `(retries_count, base_delay_secs, jitter_factor)`.
/// `backup.*` overrides `general.*` when non-zero.
pub fn effective_retries(config: &Config) -> (u32, u64, f64) {
    let retries = if config.backup.retries_on_failure > 0 {
        config.backup.retries_on_failure
    } else {
        config.general.retries_on_failure
    };

    let base_delay_secs = if !config.backup.retries_duration.is_empty() {
        parse_duration_secs(&config.backup.retries_duration).unwrap_or_else(|_| {
            tracing::warn!(
                value = %config.backup.retries_duration,
                "Invalid backup.retries_duration, using default 10s"
            );
            10
        })
    } else {
        parse_duration_secs(&config.general.retries_pause).unwrap_or_else(|_| {
            tracing::warn!(
                value = %config.general.retries_pause,
                "Invalid general.retries_pause, using default 5s"
            );
            5
        })
    };

    // backup.retries_jitter is 0.0-1.0 (fraction), general.retries_jitter is 0-100 (percent)
    let jitter = if config.backup.retries_jitter > 0.0 {
        config.backup.retries_jitter
    } else {
        config.general.retries_jitter as f64 / 100.0
    };

    (retries, base_delay_secs, jitter)
}

/// Apply jitter to a delay duration.
///
/// Returns `base_delay * (1.0 + random_fraction * jitter_factor)`.
/// Uses a simple XorShift-based PRNG seeded from the current time to avoid
/// adding a `rand` dependency.
pub fn apply_jitter(base_delay_ms: u64, jitter_factor: f64) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static JITTER_COUNTER: AtomicU64 = AtomicU64::new(0);

    if jitter_factor <= 0.0 || base_delay_ms == 0 {
        return base_delay_ms;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let seed = nanos as u64 ^ JITTER_COUNTER.fetch_add(1, Ordering::Relaxed);
    let random_fraction = (seed % 1_000_000_000) as f64 / 1_000_000_000.0;
    let jittered = base_delay_ms as f64 * (1.0 + random_fraction * jitter_factor);
    jittered as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_secs() {
        assert_eq!(parse_duration_secs("1h").unwrap(), 3600);
        assert_eq!(parse_duration_secs("24h").unwrap(), 86400);
        assert_eq!(parse_duration_secs("30m").unwrap(), 1800);
        assert_eq!(parse_duration_secs("5s").unwrap(), 5);
        assert_eq!(parse_duration_secs("10s").unwrap(), 10);
    }

    #[test]
    fn test_parse_duration_secs_public_access() {
        // Verify parse_duration_secs is public and callable from tests
        use crate::config::parse_duration_secs;
        assert_eq!(parse_duration_secs("1h").unwrap(), 3600);
        assert_eq!(parse_duration_secs("2h").unwrap(), 7200);
        assert_eq!(parse_duration_secs("45m").unwrap(), 2700);
        assert_eq!(parse_duration_secs("120s").unwrap(), 120);
    }

    #[test]
    fn test_ch_port_default_http() {
        // ClickHouse crate uses HTTP protocol (port 8123), not native TCP (port 9000)
        assert_eq!(
            default_ch_port(),
            8123,
            "default_ch_port should return 8123 for HTTP protocol"
        );

        // Verify it's wired into ClickHouseConfig default
        let config = Config::default();
        assert_eq!(config.clickhouse.port, 8123);
    }

    #[test]
    fn test_config_defaults_match_design_doc() {
        // Verify each default matches design doc §12 values
        assert_eq!(
            default_ch_timeout(),
            "5m",
            "clickhouse.timeout should be 5m per §11.7/§12"
        );
        assert_eq!(
            default_max_connections(),
            1,
            "clickhouse.max_connections should be 1 per §12 (conservative sequential default)"
        );
        assert_eq!(
            default_replica_path(),
            "/clickhouse/tables/{shard}/{database}/{table}",
            "default_replica_path should not include {{cluster}} per §12"
        );
        assert_eq!(
            default_s3_acl(),
            "",
            "s3.acl should be empty per §12 (don't send ACL header)"
        );

        // check_parts_columns defaults to false (opt-in per §12)
        let config = Config::default();
        assert!(
            !config.clickhouse.check_parts_columns,
            "check_parts_columns should default to false per §12 (opt-in)"
        );
    }

    #[test]
    fn test_env_overlay_coverage() {
        // Test all new env vars by setting them, creating a default config,
        // applying overlay, and verifying the fields were populated.
        // We use unique values that differ from defaults to confirm the overlay took effect.

        // SAFETY: single-threaded test, no concurrent env reads
        unsafe {
            // General env vars
            std::env::set_var("CHBACKUP_BACKUPS_TO_KEEP_LOCAL", "42");
            std::env::set_var("CHBACKUP_BACKUPS_TO_KEEP_REMOTE", "99");
            std::env::set_var("CHBACKUP_UPLOAD_CONCURRENCY", "16");
            std::env::set_var("CHBACKUP_DOWNLOAD_CONCURRENCY", "12");
            std::env::set_var("CHBACKUP_RETRIES_ON_FAILURE", "7");
            std::env::set_var("CHBACKUP_RETRIES_PAUSE", "30s");

            // ClickHouse env vars
            std::env::set_var("CLICKHOUSE_SECURE", "true");
            std::env::set_var("CLICKHOUSE_SKIP_VERIFY", "true");
            std::env::set_var("CLICKHOUSE_TLS_KEY", "/path/to/key.pem");
            std::env::set_var("CLICKHOUSE_TLS_CERT", "/path/to/cert.pem");
            std::env::set_var("CLICKHOUSE_TLS_CA", "/path/to/ca.pem");
            std::env::set_var("CLICKHOUSE_SYNC_REPLICATED_TABLES", "false");
            std::env::set_var("CLICKHOUSE_MAX_CONNECTIONS", "8");
            std::env::set_var("CLICKHOUSE_TIMEOUT", "15m");
            std::env::set_var("CLICKHOUSE_CONFIG_DIR", "/custom/config");
            std::env::set_var("CLICKHOUSE_DEBUG", "true");

            // S3 env vars
            std::env::set_var("S3_ACL", "bucket-owner-full-control");
            std::env::set_var("S3_STORAGE_CLASS", "GLACIER");
            std::env::set_var("S3_SSE", "aws:kms");
            std::env::set_var("S3_SSE_KMS_KEY_ID", "arn:aws:kms:us-east-1:123:key/abc");
            std::env::set_var("S3_DISABLE_SSL", "true");
            std::env::set_var("S3_DISABLE_CERT_VERIFICATION", "true");
            std::env::set_var("S3_CONCURRENCY", "5");
            std::env::set_var("S3_OBJECT_DISK_PATH", "custom/disk/path");

            // Backup env vars
            std::env::set_var("CHBACKUP_BACKUP_COMPRESSION", "zstd");
            std::env::set_var("CHBACKUP_BACKUP_UPLOAD_CONCURRENCY", "20");
            std::env::set_var("CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY", "15");
            std::env::set_var("CHBACKUP_BACKUP_RETRIES_ON_FAILURE", "10");
            std::env::set_var("CHBACKUP_BACKUP_RETRIES_DURATION", "20s");
            std::env::set_var("CHBACKUP_BACKUP_TABLES", "mydb.*");

            // API env vars
            std::env::set_var("API_SECURE", "true");
            std::env::set_var("API_USERNAME", "admin");
            std::env::set_var("API_PASSWORD", "secret123");
            std::env::set_var("API_CREATE_INTEGRATION_TABLES", "false");

            // Watch env vars
            std::env::set_var("WATCH_ENABLED", "true");
            std::env::set_var("WATCH_MAX_CONSECUTIVE_ERRORS", "10");
        }

        let mut config = Config::default();
        config.apply_env_overlay();

        // Verify General
        assert_eq!(config.general.backups_to_keep_local, 42);
        assert_eq!(config.general.backups_to_keep_remote, 99);
        assert_eq!(config.general.upload_concurrency, 16);
        assert_eq!(config.general.download_concurrency, 12);
        assert_eq!(config.general.retries_on_failure, 7);
        assert_eq!(config.general.retries_pause, "30s");

        // Verify ClickHouse
        assert!(config.clickhouse.secure);
        assert!(config.clickhouse.skip_verify);
        assert_eq!(config.clickhouse.tls_key, "/path/to/key.pem");
        assert_eq!(config.clickhouse.tls_cert, "/path/to/cert.pem");
        assert_eq!(config.clickhouse.tls_ca, "/path/to/ca.pem");
        assert!(!config.clickhouse.sync_replicated_tables);
        assert_eq!(config.clickhouse.max_connections, 8);
        assert_eq!(config.clickhouse.timeout, "15m");
        assert_eq!(config.clickhouse.config_dir, "/custom/config");
        assert!(config.clickhouse.debug);

        // Verify S3
        assert_eq!(config.s3.acl, "bucket-owner-full-control");
        assert_eq!(config.s3.storage_class, "GLACIER");
        assert_eq!(config.s3.sse, "aws:kms");
        assert_eq!(
            config.s3.sse_kms_key_id,
            "arn:aws:kms:us-east-1:123:key/abc"
        );
        assert!(config.s3.disable_ssl);
        assert!(config.s3.disable_cert_verification);
        assert_eq!(config.s3.concurrency, 5);
        assert_eq!(config.s3.object_disk_path, "custom/disk/path");

        // Verify Backup
        assert_eq!(config.backup.compression, "zstd");
        assert_eq!(config.backup.upload_concurrency, 20);
        assert_eq!(config.backup.download_concurrency, 15);
        assert_eq!(config.backup.retries_on_failure, 10);
        assert_eq!(config.backup.retries_duration, "20s");
        assert_eq!(config.backup.tables, "mydb.*");

        // Verify API
        assert!(config.api.secure);
        assert_eq!(config.api.username, "admin");
        assert_eq!(config.api.password, "secret123");
        assert!(!config.api.create_integration_tables);

        // Verify Watch
        assert!(config.watch.enabled);
        assert_eq!(config.watch.max_consecutive_errors, 10);

        // Clean up env vars to avoid polluting other tests
        // SAFETY: single-threaded test, no concurrent env reads
        unsafe {
            std::env::remove_var("CHBACKUP_BACKUPS_TO_KEEP_LOCAL");
            std::env::remove_var("CHBACKUP_BACKUPS_TO_KEEP_REMOTE");
            std::env::remove_var("CHBACKUP_UPLOAD_CONCURRENCY");
            std::env::remove_var("CHBACKUP_DOWNLOAD_CONCURRENCY");
            std::env::remove_var("CHBACKUP_RETRIES_ON_FAILURE");
            std::env::remove_var("CHBACKUP_RETRIES_PAUSE");
            std::env::remove_var("CLICKHOUSE_SECURE");
            std::env::remove_var("CLICKHOUSE_SKIP_VERIFY");
            std::env::remove_var("CLICKHOUSE_TLS_KEY");
            std::env::remove_var("CLICKHOUSE_TLS_CERT");
            std::env::remove_var("CLICKHOUSE_TLS_CA");
            std::env::remove_var("CLICKHOUSE_SYNC_REPLICATED_TABLES");
            std::env::remove_var("CLICKHOUSE_MAX_CONNECTIONS");
            std::env::remove_var("CLICKHOUSE_TIMEOUT");
            std::env::remove_var("CLICKHOUSE_CONFIG_DIR");
            std::env::remove_var("CLICKHOUSE_DEBUG");
            std::env::remove_var("S3_ACL");
            std::env::remove_var("S3_STORAGE_CLASS");
            std::env::remove_var("S3_SSE");
            std::env::remove_var("S3_SSE_KMS_KEY_ID");
            std::env::remove_var("S3_DISABLE_SSL");
            std::env::remove_var("S3_DISABLE_CERT_VERIFICATION");
            std::env::remove_var("S3_CONCURRENCY");
            std::env::remove_var("S3_OBJECT_DISK_PATH");
            std::env::remove_var("CHBACKUP_BACKUP_COMPRESSION");
            std::env::remove_var("CHBACKUP_BACKUP_UPLOAD_CONCURRENCY");
            std::env::remove_var("CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY");
            std::env::remove_var("CHBACKUP_BACKUP_RETRIES_ON_FAILURE");
            std::env::remove_var("CHBACKUP_BACKUP_RETRIES_DURATION");
            std::env::remove_var("CHBACKUP_BACKUP_TABLES");
            std::env::remove_var("API_SECURE");
            std::env::remove_var("API_USERNAME");
            std::env::remove_var("API_PASSWORD");
            std::env::remove_var("API_CREATE_INTEGRATION_TABLES");
            std::env::remove_var("WATCH_ENABLED");
            std::env::remove_var("WATCH_MAX_CONSECUTIVE_ERRORS");
        }
    }

    #[test]
    fn test_watch_config_tables_field() {
        // Verify WatchConfig has tables field with Option<String>
        let mut watch = WatchConfig::default();
        assert!(watch.tables.is_none(), "Default tables should be None");

        watch.tables = Some("default.*".to_string());
        assert_eq!(watch.tables, Some("default.*".to_string()));

        // Verify serde deserialization with tables field present
        let yaml = "enabled: true\ntables: 'default.*'";
        let config: WatchConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.tables, Some("default.*".to_string()));

        // Verify serde deserialization without tables field (backward compat)
        let yaml_no_tables = "enabled: true";
        let config2: WatchConfig = serde_yaml::from_str(yaml_no_tables).unwrap();
        assert!(config2.tables.is_none());
    }

    #[test]
    fn test_env_key_to_dot_notation_known_keys() {
        assert_eq!(env_key_to_dot_notation("S3_BUCKET"), Some("s3.bucket"));
        assert_eq!(
            env_key_to_dot_notation("CLICKHOUSE_HOST"),
            Some("clickhouse.host")
        );
        assert_eq!(env_key_to_dot_notation("API_LISTEN"), Some("api.listen"));
        assert_eq!(
            env_key_to_dot_notation("WATCH_INTERVAL"),
            Some("watch.watch_interval")
        );
        assert_eq!(
            env_key_to_dot_notation("S3_DISABLE_SSL"),
            Some("s3.disable_ssl")
        );
    }

    #[test]
    fn test_env_key_to_dot_notation_unknown_key() {
        assert_eq!(env_key_to_dot_notation("UNKNOWN_KEY"), None);
        assert_eq!(env_key_to_dot_notation("RANDOM_VAR"), None);
        assert_eq!(env_key_to_dot_notation(""), None);
    }

    #[test]
    fn test_env_key_to_dot_notation_chbackup_prefix() {
        assert_eq!(
            env_key_to_dot_notation("CHBACKUP_LOG_LEVEL"),
            Some("general.log_level")
        );
        assert_eq!(
            env_key_to_dot_notation("CHBACKUP_LOG_FORMAT"),
            Some("general.log_format")
        );
        assert_eq!(
            env_key_to_dot_notation("CHBACKUP_BACKUP_COMPRESSION"),
            Some("backup.compression")
        );
        assert_eq!(
            env_key_to_dot_notation("CHBACKUP_RETRIES_ON_FAILURE"),
            Some("general.retries_on_failure")
        );
    }

    #[test]
    fn test_cli_env_override_with_env_style_key() {
        let mut config = Config::default();
        config
            .apply_cli_env_overrides(&["S3_BUCKET=test-bucket".to_string()])
            .unwrap();
        assert_eq!(config.s3.bucket, "test-bucket");

        // Also test CLICKHOUSE_HOST
        config
            .apply_cli_env_overrides(&["CLICKHOUSE_HOST=myhost.example.com".to_string()])
            .unwrap();
        assert_eq!(config.clickhouse.host, "myhost.example.com");
    }

    #[test]
    fn test_validate_watch_intervals_always_checked() {
        // Even with watch.enabled=false, invalid intervals should fail validation.
        // This tests that the watch.enabled gate has been removed.
        let mut config = Config::default();
        config.watch.enabled = false;
        // Set watch_interval > full_interval (invalid: full must be greater)
        config.watch.watch_interval = "25h".to_string();
        config.watch.full_interval = "24h".to_string();

        let result = config.validate();
        assert!(
            result.is_err(),
            "validate() should fail when full_interval <= watch_interval even with enabled=false"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("watch.full_interval"),
            "Error should mention watch.full_interval: {}",
            err
        );
    }

    #[test]
    fn test_cli_env_override_dot_notation_still_works() {
        let mut config = Config::default();
        config
            .apply_cli_env_overrides(&["s3.bucket=dot-bucket".to_string()])
            .unwrap();
        assert_eq!(config.s3.bucket, "dot-bucket");

        config
            .apply_cli_env_overrides(&["clickhouse.host=dot-host".to_string()])
            .unwrap();
        assert_eq!(config.clickhouse.host, "dot-host");
    }

    // -----------------------------------------------------------------------
    // parse_duration_secs extended tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_duration_secs_plain_number() {
        // Plain number without suffix should be treated as seconds
        assert_eq!(parse_duration_secs("3600").unwrap(), 3600);
        assert_eq!(parse_duration_secs("0").unwrap(), 0);
        assert_eq!(parse_duration_secs("1").unwrap(), 1);
    }

    #[test]
    fn test_parse_duration_secs_whitespace() {
        // Leading/trailing whitespace should be trimmed
        assert_eq!(parse_duration_secs("  5h  ").unwrap(), 18000);
        assert_eq!(parse_duration_secs(" 30m ").unwrap(), 1800);
    }

    #[test]
    fn test_parse_duration_secs_empty_and_invalid() {
        assert!(parse_duration_secs("").is_err());
        assert!(parse_duration_secs("   ").is_err());
        assert!(parse_duration_secs("abc").is_err());
        assert!(parse_duration_secs("5x").is_err());
        assert!(parse_duration_secs("h").is_err());
        assert!(parse_duration_secs("m").is_err());
    }

    #[test]
    fn test_parse_duration_secs_large_values() {
        // 1000h should work via saturating_mul
        assert_eq!(parse_duration_secs("1000h").unwrap(), 3_600_000);
    }

    // -----------------------------------------------------------------------
    // Config::validate() API auth tests (Fix 4)
    // -----------------------------------------------------------------------

    #[test]
    fn test_validate_api_auth_both_set() {
        let mut config = Config::default();
        config.api.username = "admin".to_string();
        config.api.password = "secret".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_api_auth_both_empty() {
        let config = Config::default();
        // Default has both empty
        assert!(config.api.username.is_empty());
        assert!(config.api.password.is_empty());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_api_auth_username_only() {
        let mut config = Config::default();
        config.api.username = "admin".to_string();
        // password is empty
        let result = config.validate();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("api.username and api.password must both be set or both be empty"),
            "Expected auth validation error, got: {}",
            err
        );
    }

    #[test]
    fn test_validate_api_auth_password_only() {
        let mut config = Config::default();
        config.api.password = "secret".to_string();
        // username is empty
        let result = config.validate();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("api.username and api.password must both be set or both be empty"),
            "Expected auth validation error, got: {}",
            err
        );
    }
}
