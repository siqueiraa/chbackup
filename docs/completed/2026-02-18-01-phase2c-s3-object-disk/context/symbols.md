# Type Verification Table

## Existing Types (Verified via Source)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `PartInfo.s3_objects` | `Option<Vec<S3ObjectInfo>>` | `Option<Vec<S3ObjectInfo>>` | src/manifest.rs:139-140 |
| `S3ObjectInfo.path` | `String` | `String` | src/manifest.rs:147 |
| `S3ObjectInfo.size` | `u64` | `u64` | src/manifest.rs:150 |
| `S3ObjectInfo.backup_key` | `String` | `String` (default empty) | src/manifest.rs:153 |
| `BackupManifest.disks` | `HashMap<String, String>` | `HashMap<String, String>` | src/manifest.rs:52 |
| `BackupManifest.disk_types` | `HashMap<String, String>` | `HashMap<String, String>` | src/manifest.rs:56 |
| `TableManifest.parts` | `HashMap<String, Vec<PartInfo>>` | `HashMap<String, Vec<PartInfo>>` | src/manifest.rs:98-99 |
| `TableManifest.uuid` | `Option<String>` | `Option<String>` | src/manifest.rs:87 |
| `DiskRow.name` | `String` | `String` | src/clickhouse/client.rs:47 |
| `DiskRow.path` | `String` | `String` | src/clickhouse/client.rs:48 |
| `DiskRow.disk_type` | `String` (serde rename "type") | `String` | src/clickhouse/client.rs:49-50 |
| `Config.s3.object_disk_path` | `String` | `String` (default empty) | src/config.rs:310 |
| `Config.s3.allow_object_disk_streaming` | `bool` | `bool` (default false) | src/config.rs:314 |
| `Config.backup.object_disk_copy_concurrency` | `u32` | `u32` (default 8) | src/config.rs:347-348 |
| `Config.general.object_disk_server_side_copy_concurrency` | `u32` | `u32` (default 32) | src/config.rs:73-74 |
| `S3Client` (Clone) | `Clone` | Yes, via internal `Arc` wrapping | src/storage/s3.rs (struct has bucket: String, prefix: String, client: aws_sdk_s3::Client) |
| `ChClient` (Clone) | `Clone` | Yes | src/clickhouse/client.rs (used in spawn) |
| `TableRow.data_paths` | `Vec<String>` | `Vec<String>` | src/clickhouse/client.rs |
| `TableRow.uuid` | `String` | `String` | src/clickhouse/client.rs |
| `RateLimiter` (Clone) | `Clone` | Yes, via internal `Arc` | src/rate_limiter.rs |

## Types to Be Created

| Type | Location | Fields | Purpose |
|---|---|---|---|
| `ObjectDiskMetadata` | NEW: `src/object_disk.rs` | version: u32, objects: Vec<ObjectRef>, ref_count: u32, read_only: bool, inline_data: Option<String>, total_size: u64 | Parsed metadata from ClickHouse object disk files |
| `ObjectRef` | NEW: `src/object_disk.rs` | relative_path: String, size: u64 | Single object reference within metadata |

## S3 SDK Types (External)

| Type | Crate | Usage |
|---|---|---|
| `aws_sdk_s3::Client` | aws-sdk-s3 | Wrapped by S3Client |
| `aws_sdk_s3::types::ObjectIdentifier` | aws-sdk-s3 | Used in delete_objects |
| `aws_smithy_types::byte_stream::ByteStream` | aws-smithy-types | Used in get_object_stream |

## Key Function Signatures (Existing, Verified)

| Function | Signature | Location |
|---|---|---|
| `S3Client::put_object` | `async fn(&self, key: &str, body: Vec<u8>) -> Result<()>` | src/storage/s3.rs:160 |
| `S3Client::get_object` | `async fn(&self, key: &str) -> Result<Vec<u8>>` | src/storage/s3.rs:223 |
| `S3Client::list_objects` | `async fn(&self, prefix: &str) -> Result<Vec<S3Object>>` | src/storage/s3.rs:318 |
| `S3Client::head_object` | `async fn(&self, key: &str) -> Result<Option<u64>>` | src/storage/s3.rs:434 |
| `S3Client::full_key` | `fn(&self, relative_key: &str) -> String` | src/storage/s3.rs:147 |
| `S3Client::bucket` | `fn(&self) -> &str` | src/storage/s3.rs:132 |
| `collect_parts` | `fn(data_path: &str, freeze_name: &str, backup_dir: &Path, tables: &[TableRow]) -> Result<HashMap<String, Vec<PartInfo>>>` | src/backup/collect.rs:105 |
| `diff_parts` | `fn(current: &mut BackupManifest, base: &BackupManifest) -> DiffResult` | src/backup/diff.rs |
| `effective_upload_concurrency` | `fn(config: &Config) -> u32` | src/concurrency.rs:15 |
| `effective_download_concurrency` | `fn(config: &Config) -> u32` | src/concurrency.rs:28 |
| `effective_max_connections` | `fn(config: &Config) -> u32` | src/concurrency.rs:39 |

## Methods NOT Yet Existing (Must Be Created)

| Method | Proposed Signature | Location |
|---|---|---|
| `S3Client::copy_object` | `async fn(&self, source_bucket: &str, source_key: &str, dest_key: &str) -> Result<()>` | src/storage/s3.rs |
| `S3Client::copy_object_cross_bucket` | `async fn(&self, source_bucket: &str, source_key: &str, dest_bucket: &str, dest_key: &str) -> Result<()>` | src/storage/s3.rs |
| `parse_object_disk_metadata` | `fn(content: &str) -> Result<ObjectDiskMetadata>` | src/object_disk.rs |
| `rewrite_metadata` | `fn(metadata: &ObjectDiskMetadata, new_prefix: &str) -> String` | src/object_disk.rs |
| `effective_object_disk_copy_concurrency` | `fn(config: &Config) -> u32` | src/concurrency.rs |
