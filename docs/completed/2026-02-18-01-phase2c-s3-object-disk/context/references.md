# Symbol and Reference Analysis

## Phase 1: Symbol Verification (from source code reading + LSP)

### Types Verified (Exact Locations)

| Symbol | Kind | Location | Fields/Signature |
|--------|------|----------|-----------------|
| `BackupManifest` | struct | src/manifest.rs:19 | manifest_version, name, timestamp, clickhouse_version, chbackup_version, data_format, compressed_size, metadata_size, disks, disk_types, tables, databases, functions, named_collections, rbac |
| `TableManifest` | struct | src/manifest.rs:81 | ddl, uuid, engine, total_bytes, parts: HashMap<String, Vec<PartInfo>>, pending_mutations, metadata_only, dependencies |
| `PartInfo` | struct | src/manifest.rs:116 | name, size, backup_key, source, checksum_crc64, s3_objects: Option<Vec<S3ObjectInfo>> |
| `S3ObjectInfo` | struct | src/manifest.rs:144 | path: String, size: u64, backup_key: String |
| `S3Client` | struct | src/storage/s3.rs:25 | inner, bucket, prefix, storage_class, sse, sse_kms_key_id |
| `S3Object` | struct | src/storage/s3.rs:14 | key: String, size: i64, last_modified: Option<DateTime<Utc>> |
| `DiskRow` | struct | src/clickhouse/client.rs:46 | name, path, disk_type (#[serde(rename = "type")]) |
| `TableRow` | struct | src/clickhouse/client.rs:23 | database, name, engine, create_table_query, uuid, data_paths, total_bytes |
| `CollectedPart` | struct | src/backup/collect.rs:91 | database, table, part_info |
| `AttachParams` | struct | src/restore/attach.rs:22 | ch, db, table, parts, backup_dir, table_data_path, clickhouse_uid, clickhouse_gid |
| `OwnedAttachParams` | struct | src/restore/attach.rs:45 | ch, db, table, parts, backup_dir, table_data_path, clickhouse_uid, clickhouse_gid, engine |
| `Config` | struct | src/config.rs:8 | general, clickhouse, s3, backup, retention, watch, api |
| `S3Config` | struct | src/config.rs:244 | bucket, region, endpoint, prefix, ..., object_disk_path, allow_object_disk_streaming, ... |
| `BackupConfig` | struct | src/config.rs:326 | ..., object_disk_copy_concurrency, ... |
| `GeneralConfig` | struct | src/config.rs:36 | ..., object_disk_server_side_copy_concurrency, ... |
| `RateLimiter` | struct | src/rate_limiter.rs | (internal Arc) - Clone |

### Functions Verified (Exact Signatures)

| Function | Signature | Location |
|----------|-----------|----------|
| `collect_parts` | `fn(data_path: &str, freeze_name: &str, backup_dir: &Path, tables: &[TableRow]) -> Result<HashMap<String, Vec<PartInfo>>>` | src/backup/collect.rs:105 |
| `diff_parts` | `fn(current: &mut BackupManifest, base: &BackupManifest) -> DiffResult` | src/backup/diff.rs:29 |
| `effective_upload_concurrency` | `fn(config: &Config) -> u32` | src/concurrency.rs:15 |
| `effective_download_concurrency` | `fn(config: &Config) -> u32` | src/concurrency.rs:28 |
| `effective_max_connections` | `fn(config: &Config) -> u32` | src/concurrency.rs:39 |
| `freeze_name` | `fn(backup_name: &str, db: &str, table: &str) -> String` | src/clickhouse/client.rs:392 |
| `compute_crc64` | `fn(path: &Path) -> Result<u64>` | src/backup/checksum.rs |
| `url_encode_path` | `fn(s: &str) -> String` | src/backup/collect.rs:24 |
| `attach_parts_owned` | `async fn(params: OwnedAttachParams) -> Result<u64>` | src/restore/attach.rs:70 |
| `detect_clickhouse_ownership` | `fn(data_path: &Path) -> Result<(Option<u32>, Option<u32>)>` | src/restore/attach.rs:331 |
| `get_table_data_path` | `fn(data_paths: &[String], data_path_config: &str, db: &str, table: &str) -> PathBuf` | src/restore/attach.rs:365 |

### S3Client Methods (Complete Inventory)

| Method | Signature | Notes |
|--------|-----------|-------|
| `new` | `async fn(config: &S3Config) -> Result<Self>` | Async constructor |
| `ping` | `async fn(&self) -> Result<()>` | ListObjectsV2 max_keys=1 |
| `inner` | `fn(&self) -> &aws_sdk_s3::Client` | Underlying SDK client |
| `bucket` | `fn(&self) -> &str` | Configured bucket |
| `prefix` | `fn(&self) -> &str` | Configured prefix |
| `full_key` | `fn(&self, relative_key: &str) -> String` | Prepend prefix |
| `put_object` | `async fn(&self, key: &str, body: Vec<u8>) -> Result<()>` | Basic upload |
| `put_object_with_options` | `async fn(&self, key: &str, body: Vec<u8>, content_type: Option<&str>) -> Result<()>` | Upload with SSE/SC |
| `get_object` | `async fn(&self, key: &str) -> Result<Vec<u8>>` | Full download to memory |
| `get_object_stream` | `async fn(&self, key: &str) -> Result<ByteStream>` | Streaming download |
| `list_common_prefixes` | `async fn(&self, prefix: &str, delimiter: &str) -> Result<Vec<String>>` | Directory listing |
| `list_objects` | `async fn(&self, prefix: &str) -> Result<Vec<S3Object>>` | Object listing with pagination |
| `delete_object` | `async fn(&self, key: &str) -> Result<()>` | Single delete |
| `delete_objects` | `async fn(&self, keys: Vec<String>) -> Result<()>` | Batch delete (1000 per batch) |
| `head_object` | `async fn(&self, key: &str) -> Result<Option<u64>>` | Check existence + size |
| `create_multipart_upload` | `async fn(&self, key: &str) -> Result<String>` | Initiate multipart |
| `upload_part` | `async fn(&self, key: &str, upload_id: &str, part_number: i32, body: Vec<u8>) -> Result<String>` | Upload chunk |
| `complete_multipart_upload` | `async fn(&self, key: &str, upload_id: &str, parts: Vec<(i32, String)>) -> Result<()>` | Finalize multipart |
| `abort_multipart_upload` | `async fn(&self, key: &str, upload_id: &str) -> Result<()>` | Cancel multipart |
| **MISSING: `copy_object`** | N/A | Must be added for Phase 2c |

## Phase 1.5: Call Hierarchy and Reference Analysis

### `collect_parts` References

| Caller | File:Line | Usage |
|--------|-----------|-------|
| Import | src/backup/mod.rs:36 | `use self::collect::collect_parts;` |
| Call | src/backup/mod.rs:277 | `collect_parts(&data_path, &fname_for_collect, &backup_dir_clone, &tables_for_collect)` |
| Comment | src/upload/mod.rs:452 | `Uses URL-encoded paths to match what collect_parts creates.` |

**Impact**: `collect_parts` is called from ONE location in `backup/mod.rs`. Any signature change must update that single call site. The function runs inside `spawn_blocking` so must remain sync.

### `effective_*` Concurrency Functions References

| Function | Callers |
|----------|---------|
| `effective_upload_concurrency` | src/upload/mod.rs:239 |
| `effective_download_concurrency` | src/download/mod.rs:135 |
| `effective_max_connections` | src/backup/mod.rs:193, src/restore/mod.rs:199 |

**Impact**: New `effective_object_disk_copy_concurrency` follows same pattern, no existing callers to change.

### `s3_objects` Field References

All usages set `s3_objects: None` except in test fixtures (manifest.rs test_manifest_matches_design_doc_example). The field is always skipped when None via `skip_serializing_if`. This field will be populated by Phase 2c for S3 disk parts.

**Files that construct PartInfo with `s3_objects: None`:**
- src/backup/collect.rs:225
- src/backup/diff.rs:108
- src/upload/mod.rs:508
- src/download/mod.rs:288
- src/restore/sort.rs:182

### `disk_type_map` / `disk_types` References

- src/backup/mod.rs:87-90 -- builds `disk_type_map` from `disks` query results
- src/backup/mod.rs:389 -- stores into `manifest.disk_types`
- src/manifest.rs:56 -- `pub disk_types: HashMap<String, String>`
- src/config.rs:226 -- `skip_disk_types` config field

**Impact**: The disk_type_map is already built and stored in the manifest. Phase 2c will USE it to determine S3 disk routing.

### `object_disk` Config References

All config fields exist and are wired into `set_field` for CLI override:
- `s3.object_disk_path` (src/config.rs:310) -- default empty string
- `s3.allow_object_disk_streaming` (src/config.rs:314) -- default false
- `backup.object_disk_copy_concurrency` (src/config.rs:347) -- default 8
- `general.object_disk_server_side_copy_concurrency` (src/config.rs:74) -- default 32

### `OwnedAttachParams` / `attach_parts_owned` References

| Usage | File:Line |
|-------|-----------|
| Definition | src/restore/attach.rs:45 (struct), :70 (fn) |
| Import | src/restore/mod.rs:31 |
| Construction | src/restore/mod.rs:185 |
| Call | src/restore/mod.rs:221 |

**Impact**: Phase 2c may need to add S3-related fields to `OwnedAttachParams` (e.g., `S3Client`, `disk_type_map`) for S3 disk restore.

### Hardcoded "default" Disk Name

**Critical finding**: `src/backup/mod.rs:293` hardcodes `parts_by_disk.insert("default".to_string(), parts_for_table)`. This means ALL parts from the shadow walk are placed under the "default" disk key, regardless of which disk they actually belong to. Phase 2c MUST change this to route parts by their actual disk name.

## Phase 2: Design Doc Cross-References

### Design Doc Sections Consumed by Phase 2c

| Section | Content | Key Details |
|---------|---------|-------------|
| 3.4 (line 1077-1088) | Shadow walk disk routing | S3 disk: parse metadata -> collect S3 keys; CopyObject parallel |
| 3.6 (line 1168-1175) | Upload: S3 disk separate semaphore | `object_disk_copy_concurrency` semaphore, CopyObject server-side |
| 3.7 (line 1197-1223) | Metadata parsing (5 formats) | Version 1-5 format, InlineData (v4), FullObjectKey (v5) |
| 4 (line 1268-1270) | Download: S3 disk metadata only | CopyObject metadata files, actual objects stay in backup bucket |
| 5.3 (line 1366-1411) | Restore: S3 copies parallel per table | CopyObject before ATTACH, separate semaphore |
| 5.4 (line 1417-1489) | UUID isolation for restore | Always new paths, same-name optimization, metadata rewrite |
| 16.2 (line 2762-2775) | Mixed disk handling | Per-part routing, detect via `type = 's3' OR type = 'object_storage'` |
