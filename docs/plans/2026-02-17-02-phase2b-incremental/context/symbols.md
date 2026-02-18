# Type Verification -- Phase 2b Incremental Backups

## Types Used in This Plan

| Variable/Field | Assumed Type | Actual Type | Verification Location |
|---|---|---|---|
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | src/manifest.rs:60 |
| `BackupManifest.name` | `String` | `String` | src/manifest.rs:25 |
| `TableManifest.parts` | `HashMap<String, Vec<PartInfo>>` | `HashMap<String, Vec<PartInfo>>` | src/manifest.rs:99 |
| `PartInfo.name` | `String` | `String` | src/manifest.rs:118 |
| `PartInfo.size` | `u64` | `u64` | src/manifest.rs:121 |
| `PartInfo.backup_key` | `String` | `String` | src/manifest.rs:126 |
| `PartInfo.source` | `String` | `String` (default "uploaded") | src/manifest.rs:131 |
| `PartInfo.checksum_crc64` | `u64` | `u64` (default 0) | src/manifest.rs:135 |
| `PartInfo.s3_objects` | `Option<Vec<S3ObjectInfo>>` | `Option<Vec<S3ObjectInfo>>` | src/manifest.rs:140 |
| `S3Client.get_object(key)` | `-> Result<Vec<u8>>` | `-> Result<Vec<u8>>` | src/storage/s3.rs:223 |
| `BackupManifest::from_json_bytes(data)` | `-> Result<Self>` | `-> Result<Self>` | src/manifest.rs:228 |
| `BackupManifest::load_from_file(path)` | `-> Result<Self>` | `-> Result<Self>` | src/manifest.rs:219 |
| `BackupManifest::save_to_file(path)` | `-> Result<()>` | `-> Result<()>` | src/manifest.rs:206 |
| `Config.clickhouse.data_path` | `String` | `String` | src/config.rs:112 |
| `Config.backup.compression` | `String` | `String` | src/config.rs:336 |
| `Config.backup.upload_max_bytes_per_second` | `u64` | `u64` | src/config.rs:352 |
| `backup::create()` signature | `(config, ch, name, pattern, schema_only) -> Result<BackupManifest>` | `(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool) -> Result<BackupManifest>` | src/backup/mod.rs:41-47 |
| `upload::upload()` signature | `(config, s3, name, dir, delete_local) -> Result<()>` | `(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool) -> Result<()>` | src/upload/mod.rs:96-102 |
| `S3Client::new(config)` | `async -> Result<Self>` | `pub async fn new(config: &S3Config) -> Result<Self>` | src/storage/s3.rs:44 |
| `ChClient::new(config)` | `-> Result<Self>` | `pub fn new(config: &ClickHouseConfig) -> Result<Self>` | verified via grep |
| `Command::Create.diff_from` | `Option<String>` | `Option<String>` | src/cli.rs:47 |
| `Command::Upload.diff_from_remote` | `Option<String>` | `Option<String>` | src/cli.rs:89 |
| `Command::CreateRemote.diff_from_remote` | `Option<String>` | `Option<String>` | src/cli.rs:176 |

## Key Functions Referenced

| Function | Location | Signature |
|---|---|---|
| `backup::create` | src/backup/mod.rs:41 | `pub async fn create(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool) -> Result<BackupManifest>` |
| `upload::upload` | src/upload/mod.rs:96 | `pub async fn upload(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool) -> Result<()>` |
| `S3Client::get_object` | src/storage/s3.rs:223 | `pub async fn get_object(&self, key: &str) -> Result<Vec<u8>>` |
| `S3Client::put_object` | src/storage/s3.rs:160 | `pub async fn put_object(&self, key: &str, body: Vec<u8>) -> Result<()>` |
| `compute_crc64` | src/backup/checksum.rs:17 | `pub fn compute_crc64(path: &Path) -> Result<u64>` |
| `compute_crc64_bytes` | src/backup/checksum.rs:38 | `pub fn compute_crc64_bytes(data: &[u8]) -> u64` |
| `collect_parts` | src/backup/collect.rs:105 | `pub fn collect_parts(data_path: &str, freeze_name: &str, backup_dir: &Path, tables: &[TableRow]) -> Result<HashMap<String, Vec<PartInfo>>>` |
| `s3_key_for_part` | src/upload/mod.rs:60 | `fn s3_key_for_part(backup_name: &str, db: &str, table: &str, part_name: &str) -> String` |
| `effective_upload_concurrency` | src/concurrency.rs:15 | `pub fn effective_upload_concurrency(config: &Config) -> u32` |

## Type Safety Notes

- `PartInfo.source` is a plain `String`, not an enum. Values are `"uploaded"` or `"carried:{base_name}"`. Use `starts_with("carried:")` to detect carried parts.
- `PartInfo.checksum_crc64` defaults to `0` via serde. A CRC64 of 0 means "empty data" (CRC64/XZ of empty bytes is 0). This is a valid edge case for parts with empty checksums.txt.
- `BackupManifest.tables` key format is `"db.table"` (dot-separated).
- `TableManifest.parts` key is disk name (e.g., `"default"`, `"s3disk"`).
