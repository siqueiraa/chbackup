# Type Verification Table

## Config Types (Concurrency Parameters)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `config.backup.upload_concurrency` | `u32` | `u32` | config.rs:341 |
| `config.backup.download_concurrency` | `u32` | `u32` | config.rs:345 |
| `config.clickhouse.max_connections` | `u32` | `u32` | config.rs:166 |
| `config.backup.upload_max_bytes_per_second` | `u64` | `u64` | config.rs:352 |
| `config.backup.download_max_bytes_per_second` | `u64` | `u64` | config.rs:356 |
| `config.s3.max_parts_count` | `u32` | `u32` | config.rs:298 |
| `config.s3.chunk_size` | `u64` | `u64` | config.rs:302 |
| `config.backup.object_disk_copy_concurrency` | `u32` | `u32` | config.rs:348 |
| `config.general.upload_concurrency` | `u32` | `u32` | config.rs:59 |
| `config.general.download_concurrency` | `u32` | `u32` | config.rs:63 |

## Core Types

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | manifest.rs:60 |
| `TableManifest.parts` | `HashMap<String, Vec<PartInfo>>` | `HashMap<String, Vec<PartInfo>>` | manifest.rs:99 |
| `TableManifest.engine` | `String` | `String` | manifest.rs:91 |
| `PartInfo.name` | `String` | `String` | manifest.rs:117 |
| `PartInfo.size` | `u64` | `u64` | manifest.rs:121 |
| `PartInfo.backup_key` | `String` | `String` | manifest.rs:125 |
| `PartInfo.source` | `String` | `String` | manifest.rs:131 |
| `PartInfo.checksum_crc64` | `u64` | `u64` | manifest.rs:135 |
| `ChClient` | `struct` | `struct { inner: clickhouse::Client, host: String, port: u16, log_sql_queries: bool }` | client.rs:12-19 |
| `S3Client` | `struct` | `struct { inner: aws_sdk_s3::Client, bucket: String, prefix: String, storage_class: String, sse: String, sse_kms_key_id: String }` | s3.rs:23-35 |
| `FreezeGuard.frozen` | `Vec<FreezeInfo>` | `Vec<FreezeInfo>` | freeze.rs:24 |
| `FreezeInfo` | `struct` | `{ database: String, table: String, freeze_name: String }` | freeze.rs:14-18 |
| `TableRow` | `struct` | `{ database: String, name: String, engine: String, create_table_query: String, uuid: String, data_paths: Vec<String>, total_bytes: Option<u64> }` | client.rs:22-31 |

## Key Function Signatures

| Function | Signature | Location |
|---|---|---|
| `backup::create` | `async fn(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool) -> Result<BackupManifest>` | backup/mod.rs:37-43 |
| `upload::upload` | `async fn(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool) -> Result<()>` | upload/mod.rs:66-72 |
| `download::download` | `async fn(config: &Config, s3: &S3Client, backup_name: &str) -> Result<PathBuf>` | download/mod.rs:49-53 |
| `restore::restore` | `async fn(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool, data_only: bool) -> Result<()>` | restore/mod.rs:43-50 |
| `freeze::freeze_table` | `async fn(ch: &ChClient, guard: &mut FreezeGuard, db: &str, table: &str, freeze_name: &str, ignore_not_exists: bool) -> Result<bool>` | freeze.rs:104-111 |
| `stream::compress_part` | `fn(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>>` | upload/stream.rs:16 |
| `stream::decompress_part` | `fn(data: &[u8], output_dir: &Path) -> Result<()>` | download/stream.rs:16 |
| `sort::sort_parts_by_min_block` | `fn(parts: &[PartInfo]) -> Vec<PartInfo>` | restore/sort.rs:64 |
| `sort::needs_sequential_attach` | `fn(engine: &str) -> bool` | restore/sort.rs:83 |
| `s3.put_object` | `async fn(&self, key: &str, body: Vec<u8>) -> Result<()>` | s3.rs:158 |
| `s3.get_object` | `async fn(&self, key: &str) -> Result<Vec<u8>>` | s3.rs:221 |
| `s3.get_object_stream` | `async fn(&self, key: &str) -> Result<ByteStream>` | s3.rs:249 |
| `ch.freeze_table` | `async fn(&self, db: &str, table: &str, freeze_name: &str) -> Result<()>` | client.rs:139-144 |
| `ch.unfreeze_table` | `async fn(&self, db: &str, table: &str, freeze_name: &str) -> Result<()>` | client.rs:153-163 |
| `ch.attach_part` | `async fn(&self, db: &str, table: &str, part_name: &str) -> Result<()>` | client.rs:275-286 |
| `attach::attach_parts` | `async fn(params: &AttachParams<'_>) -> Result<u64>` | restore/attach.rs:43 |

## New Types Needed for Phase 2a

| Type | Purpose | Fields |
|---|---|---|
| `tokio::sync::Semaphore` | Concurrency limiter | Built-in tokio type, needs `Arc<Semaphore>` |
| `futures::future::try_join_all` | Fail-fast join | New crate dependency needed |
| Multipart upload methods on S3Client | `create_multipart_upload`, `upload_part`, `complete_multipart_upload`, `abort_multipart_upload` | Methods to add to `S3Client` |

## Anti-Pattern Checks

| Pattern | Status |
|---|---|
| `.as_str()` on enum types | N/A -- no custom enums involved in parallelism |
| Implicit String to Enum | N/A -- all concurrency params are numeric (u32/u64) |
| Tuple field order assumption | N/A -- no tuple types in the parallelism flow |
