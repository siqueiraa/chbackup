# Type Verification Table

All types verified against actual source code via grep.

## Config Types

| Symbol | Type | Location | Verified |
|--------|------|----------|----------|
| `Config` | struct | `src/config.rs:8` | grep confirmed |
| `Config.general` | `GeneralConfig` | `src/config.rs:10` | field confirmed |
| `Config.clickhouse` | `ClickHouseConfig` | `src/config.rs:13` | field confirmed |
| `Config.s3` | `S3Config` | `src/config.rs:16` | field confirmed |
| `Config.backup` | `BackupConfig` | `src/config.rs:19` | field confirmed |
| `Config.retention` | `RetentionConfig` | `src/config.rs:22` | field confirmed |
| `Config.watch` | `WatchConfig` | `src/config.rs:25` | field confirmed |
| `Config.api` | `ApiConfig` | `src/config.rs:28` | field confirmed |
| `ApiConfig` | struct | `src/config.rs:427` | grep confirmed |
| `ApiConfig.listen` | `String` | `src/config.rs:429` | default "localhost:7171" |
| `ApiConfig.enable_metrics` | `bool` | `src/config.rs:432` | default true |
| `ApiConfig.create_integration_tables` | `bool` | `src/config.rs:436` | default true |
| `ApiConfig.integration_tables_host` | `String` | `src/config.rs:440` | default "" |
| `ApiConfig.username` | `String` | `src/config.rs:444` | default "" |
| `ApiConfig.password` | `String` | `src/config.rs:448` | default "" |
| `ApiConfig.secure` | `bool` | `src/config.rs:452` | default false |
| `ApiConfig.certificate_file` | `String` | `src/config.rs:456` | default "" |
| `ApiConfig.private_key_file` | `String` | `src/config.rs:460` | default "" |
| `ApiConfig.ca_cert_file` | `String` | `src/config.rs:464` | default "" |
| `ApiConfig.allow_parallel` | `bool` | `src/config.rs:468` | default false |
| `ApiConfig.complete_resumable_after_restart` | `bool` | `src/config.rs:472` | default true |
| `ApiConfig.watch_is_main_process` | `bool` | `src/config.rs:476` | default false |
| `WatchConfig` | struct | `src/config.rs:393` | grep confirmed |
| `WatchConfig.enabled` | `bool` | `src/config.rs:396` | default false |

## Client Types

| Symbol | Type | Location | Verified |
|--------|------|----------|----------|
| `ChClient` | struct (Clone) | `src/clickhouse/client.rs:12` | grep confirmed |
| `ChClient::new` | `fn(&ClickHouseConfig) -> Result<Self>` | `src/clickhouse/client.rs:91` | grep confirmed |
| `ChClient::execute_ddl` | `async fn(&self, &str) -> Result<()>` | `src/clickhouse/client.rs` | grep confirmed |
| `S3Client` | struct | `src/storage/s3.rs:25` | grep confirmed |
| `S3Client::new` | `async fn(&S3Config) -> Result<Self>` | `src/storage/s3.rs:44` | grep confirmed |
| `PidLock` | struct | `src/lock.rs:27` | grep confirmed |
| `PidLock::acquire` | `fn(&Path, &str) -> Result<Self, ChBackupError>` | `src/lock.rs:36` | grep confirmed |

## Command Entry Points

| Symbol | Signature | Location | Verified |
|--------|-----------|----------|----------|
| `backup::create` | `async fn(&Config, &ChClient, &str, Option<&str>, bool, Option<&str>, Option<&str>, bool) -> Result<BackupManifest>` | `src/backup/mod.rs:64` | grep confirmed |
| `upload::upload` | `async fn(&Config, &S3Client, &str, &Path, bool, Option<&str>, bool) -> Result<()>` | `src/upload/mod.rs:165` | grep confirmed |
| `download::download` | `async fn(&Config, &S3Client, &str, bool) -> Result<PathBuf>` | `src/download/mod.rs:136` | grep confirmed |
| `restore::restore` | `async fn(&Config, &ChClient, &str, Option<&str>, bool, bool, bool) -> Result<()>` | `src/restore/mod.rs:57` | grep confirmed |
| `list::list_local` | `fn(&str) -> Result<Vec<BackupSummary>>` | `src/list.rs:81` | grep confirmed |
| `list::list_remote` | `async fn(&S3Client) -> Result<Vec<BackupSummary>>` | `src/list.rs:125` | grep confirmed |
| `list::delete` | `async fn(&str, &S3Client, &Location, &str) -> Result<()>` | `src/list.rs:202` | grep confirmed |
| `list::delete_local` | `fn(&str, &str) -> Result<()>` | `src/list.rs:217` | grep confirmed |
| `list::delete_remote` | `async fn(&S3Client, &str) -> Result<()>` | `src/list.rs:248` | grep confirmed |
| `list::clean_broken` | `async fn(&str, &S3Client, &Location) -> Result<()>` | `src/list.rs:356` | grep confirmed |
| `list::clean_broken_local` | `fn(&str) -> Result<usize>` | `src/list.rs:292` | grep confirmed |
| `list::clean_broken_remote` | `async fn(&S3Client) -> Result<usize>` | `src/list.rs:325` | grep confirmed |

## Data Types

| Symbol | Type | Location | Verified |
|--------|------|----------|----------|
| `BackupManifest` | struct (Serialize, Deserialize) | `src/manifest.rs:19` | grep confirmed |
| `BackupSummary` | struct | `src/list.rs:25` | grep confirmed |
| `BackupSummary.name` | `String` | `src/list.rs:27` | field confirmed |
| `BackupSummary.timestamp` | `Option<DateTime<Utc>>` | `src/list.rs:29` | field confirmed |
| `BackupSummary.size` | `u64` | `src/list.rs:31` | field confirmed |
| `BackupSummary.compressed_size` | `u64` | `src/list.rs:33` | field confirmed |
| `BackupSummary.table_count` | `usize` | `src/list.rs:35` | field confirmed |
| `BackupSummary.is_broken` | `bool` | `src/list.rs:37` | field confirmed |
| `BackupSummary.broken_reason` | `Option<String>` | `src/list.rs:40` | field confirmed |
| `list::Location` | enum (Local, Remote) | `src/list.rs:17-21` | grep confirmed |
| `UploadState` | struct (Serialize, Deserialize) | `src/resume.rs:23` | grep confirmed |
| `DownloadState` | struct (Serialize, Deserialize) | `src/resume.rs:37` | grep confirmed |
| `RestoreState` | struct (Serialize, Deserialize) | `src/resume.rs:51` | grep confirmed |

## Lock Types

| Symbol | Type | Location | Verified |
|--------|------|----------|----------|
| `LockScope` | enum (Backup(String), Global, None) | `src/lock.rs:101` | grep confirmed |
| `lock_for_command` | `fn(&str, Option<&str>) -> LockScope` | `src/lock.rs:116` | grep confirmed |
| `lock_path_for_scope` | `fn(&LockScope) -> Option<PathBuf>` | `src/lock.rs:133` | grep confirmed |

## Resume Helpers

| Symbol | Signature | Location | Verified |
|--------|-----------|----------|----------|
| `resume::load_state_file` | `fn<T: DeserializeOwned>(&Path) -> Result<Option<T>>` | `src/resume.rs:87` | grep confirmed |
| `resume::save_state_graceful` | `fn<T: Serialize>(&Path, &T)` | `src/resume.rs:106` | grep confirmed |
| `resume::delete_state_file` | `fn(&Path)` | `src/resume.rs:134` | grep confirmed |

## Logging

| Symbol | Signature | Location | Verified |
|--------|-----------|----------|----------|
| `logging::init_logging` | `fn(&str, &str, bool)` | `src/logging.rs:13` | grep confirmed |

## New Dependencies Needed

| Crate | Version | Purpose |
|-------|---------|---------|
| `axum` | 0.7 | HTTP framework (already in Cargo.toml roadmap) |
| `tower-http` | 0.5 | CORS, compression, auth middleware layers |
| `tokio-util` | 0.7 | CancellationToken (already a dependency for codec) |
| `axum-server` | 0.7 | TLS support for axum (rustls backend) |
| `base64` | 0.22 | Basic auth header decoding |
| `hyper` | 1.0 | Transitive via axum, may need for lower-level server control |
