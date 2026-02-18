# Type Verification

## Verified Types

| Variable/Field | Assumed Type | Actual Type | Verification Source |
|---|---|---|---|
| `Config` | top-level config | struct with 7 sections | `src/config.rs:8` |
| `Config.general` | GeneralConfig | `GeneralConfig` struct | `src/config.rs:10` |
| `Config.clickhouse` | ClickHouseConfig | `ClickHouseConfig` struct | `src/config.rs:13` |
| `Config.s3` | S3Config | `S3Config` struct | `src/config.rs:16` |
| `Config.backup` | BackupConfig | `BackupConfig` struct | `src/config.rs:19` |
| `GeneralConfig.use_resumable_state` | bool | `bool` | `src/config.rs:91` |
| `GeneralConfig.retries_on_failure` | u32 | `u32` | `src/config.rs:78` |
| `ClickHouseConfig.secure` | bool | `bool` | `src/config.rs:119` |
| `ClickHouseConfig.skip_verify` | bool | `bool` | `src/config.rs:122` |
| `ClickHouseConfig.tls_key` | String | `String` | `src/config.rs:127` |
| `ClickHouseConfig.tls_cert` | String | `String` | `src/config.rs:131` |
| `ClickHouseConfig.tls_ca` | String | `String` | `src/config.rs:135` |
| `ClickHouseConfig.check_parts_columns` | bool | `bool` | `src/config.rs:147` |
| `ClickHouseConfig.skip_disks` | Vec<String> | `Vec<String>` | `src/config.rs:222` |
| `ClickHouseConfig.skip_disk_types` | Vec<String> | `Vec<String>` | `src/config.rs:226` |
| `BackupConfig.retries_on_failure` | u32 | `u32` | `src/config.rs:359` |
| `BackupManifest` | struct | `struct BackupManifest` | `src/manifest.rs:19` |
| `BackupManifest.name` | String | `String` | `src/manifest.rs:26` |
| `BackupManifest.tables` | HashMap<String, TableManifest> | `HashMap<String, TableManifest>` | `src/manifest.rs:65` |
| `BackupManifest.disk_types` | HashMap<String, String> | `HashMap<String, String>` | `src/manifest.rs:56` |
| `TableManifest.parts` | HashMap<String, Vec<PartInfo>> | `HashMap<String, Vec<PartInfo>>` | `src/manifest.rs:104` |
| `PartInfo.name` | String | `String` | `src/manifest.rs:123` |
| `PartInfo.size` | u64 | `u64` | `src/manifest.rs:127` |
| `PartInfo.backup_key` | String | `String` | `src/manifest.rs:131` |
| `PartInfo.source` | String | `String` | `src/manifest.rs:136` |
| `PartInfo.checksum_crc64` | u64 | `u64` | `src/manifest.rs:140` |
| `PartInfo.s3_objects` | Option<Vec<S3ObjectInfo>> | `Option<Vec<S3ObjectInfo>>` | `src/manifest.rs:145` |
| `S3ObjectInfo.path` | String | `String` | `src/manifest.rs:152` |
| `S3ObjectInfo.size` | u64 | `u64` | `src/manifest.rs:155` |
| `S3ObjectInfo.backup_key` | String | `String` | `src/manifest.rs:159` |
| `BackupSummary.is_broken` | bool | `bool` | `src/list.rs:37` |
| `BackupSummary.name` | String | `String` | `src/list.rs:27` |
| `ChClient` | struct | `struct ChClient` (Clone) | `src/clickhouse/client.rs:12` |
| `ChClient.log_sql_queries` | bool | `bool` | `src/clickhouse/client.rs:18` |
| `TableRow.uuid` | String | `String` | `src/clickhouse/client.rs:28` |
| `TableRow.data_paths` | Vec<String> | `Vec<String>` | `src/clickhouse/client.rs:29` |
| `DiskRow.name` | String | `String` | `src/clickhouse/client.rs:47` |
| `DiskRow.path` | String | `String` | `src/clickhouse/client.rs:48` |
| `DiskRow.disk_type` | String | `String` (serde rename "type") | `src/clickhouse/client.rs:50` |
| `DiskRow.remote_path` | String | `String` (default empty) | `src/clickhouse/client.rs:53` |
| `S3Client` | struct | `struct S3Client` (Clone, Debug) | `src/storage/s3.rs:25` |
| `S3Client.bucket` | String | `String` | `src/storage/s3.rs:28` |
| `S3Client.prefix` | String | `String` | `src/storage/s3.rs:30` |
| `S3Object.key` | String | `String` | `src/storage/s3.rs:14` |
| `S3Object.size` | i64 | `i64` | `src/storage/s3.rs:15` |
| `OwnedAttachParams` | struct | struct with many fields | `src/restore/attach.rs` |
| `CollectedPart.disk_name` | String | `String` | `src/backup/collect.rs` |
| `ChBackupError` | enum | `enum ChBackupError` (thiserror) | `src/error.rs:4` |
| `RateLimiter` | struct (Clone) | `struct RateLimiter` (Clone via Arc) | `src/rate_limiter.rs` |

## New Types to Define (Phase 2d)

| Type | Purpose | Fields |
|---|---|---|
| `UploadState` | Resume state for upload | `completed_parts: HashSet<String>`, `backup_name: String`, `params_hash: String` |
| `DownloadState` | Resume state for download | `completed_parts: HashSet<String>`, `backup_name: String`, `params_hash: String` |
| `RestoreState` | Resume state for restore | `attached_parts: HashMap<String, Vec<String>>`, `backup_name: String` |

## Verified Functions

| Function | Signature | Location |
|---|---|---|
| `compute_crc64` | `fn(path: &Path) -> Result<u64>` | `src/backup/checksum.rs:17` |
| `compute_crc64_bytes` | `fn(data: &[u8]) -> u64` | `src/backup/checksum.rs:38` |
| `is_s3_disk` | `fn(disk_type: &str) -> bool` | `src/object_disk.rs` |
| `diff_parts` | `fn(current: &mut BackupManifest, base: &BackupManifest) -> DiffResult` | `src/backup/diff.rs` |
| `collect_parts` | accepts `disk_type_map` and `disk_paths` | `src/backup/collect.rs` |
| `BackupManifest::save_to_file` | `fn(&self, path: &Path) -> Result<()>` | `src/manifest.rs:211` |
| `BackupManifest::load_from_file` | `fn(path: &Path) -> Result<Self>` | `src/manifest.rs:224` |
| `BackupManifest::from_json_bytes` | `fn(data: &[u8]) -> Result<Self>` | `src/manifest.rs:233` |
| `BackupManifest::to_json_bytes` | `fn(&self) -> Result<Vec<u8>>` | `src/manifest.rs:240` |
| `S3Client::put_object` | `async fn(&self, key: &str, body: Vec<u8>) -> Result<()>` | `src/storage/s3.rs` |
| `S3Client::put_object_with_options` | `async fn(&self, key: &str, body: Vec<u8>, content_type: Option<&str>) -> Result<()>` | `src/storage/s3.rs` |
| `S3Client::get_object` | `async fn(&self, key: &str) -> Result<Vec<u8>>` | `src/storage/s3.rs` |
| `S3Client::copy_object` | `async fn(&self, source_bucket: &str, source_key: &str, dest_key: &str) -> Result<()>` | `src/storage/s3.rs` |
| `S3Client::copy_object_with_retry` | `async fn(&self, source_bucket: &str, source_key: &str, dest_key: &str, allow_streaming: bool) -> Result<()>` | `src/storage/s3.rs` |
| `S3Client::delete_object` | `async fn(&self, key: &str) -> Result<()>` | `src/storage/s3.rs` |
| `S3Client::list_objects` | `async fn(&self, prefix: &str) -> Result<Vec<S3Object>>` | `src/storage/s3.rs` |
| `S3Client::list_common_prefixes` | `async fn(&self, prefix: &str, delimiter: &str) -> Result<Vec<String>>` | `src/storage/s3.rs` |
| `S3Client::head_object` | `async fn(&self, key: &str) -> Result<Option<u64>>` | `src/storage/s3.rs` |
| `S3Client::bucket` | `fn(&self) -> &str` | `src/storage/s3.rs` |
| `S3Client::prefix` | `fn(&self) -> &str` | `src/storage/s3.rs` |
| `S3Client::full_key` | `fn(&self, relative_key: &str) -> String` | `src/storage/s3.rs` |
| `ChClient::new` | `fn(config: &ClickHouseConfig) -> Result<Self>` | `src/clickhouse/client.rs:61` |
| `ChClient::freeze_table` | `async fn` | `src/clickhouse/client.rs` |
| `ChClient::list_tables` | `async fn(&self) -> Result<Vec<TableRow>>` | `src/clickhouse/client.rs` |
| `ChClient::get_disks` | `async fn(&self) -> Result<Vec<DiskRow>>` | `src/clickhouse/client.rs` |
| `ChClient::execute_ddl` | `async fn(&self, ddl: &str) -> Result<()>` | `src/clickhouse/client.rs` |
| `ChClient::attach_part` | `async fn(&self, db: &str, table: &str, part_name: &str) -> Result<()>` | `src/clickhouse/client.rs` |
