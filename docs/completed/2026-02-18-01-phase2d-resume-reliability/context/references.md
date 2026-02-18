# References - Phase 2d Resume & Reliability

## Symbol Analysis

### Core Types and Structs

#### BackupManifest (`src/manifest.rs`)
- Central data structure flowing between all commands
- Serialized as `metadata.json` in each backup directory
- Fields: `manifest_version`, `name`, `timestamp`, `clickhouse_version`, `chbackup_version`, `data_format`, `compressed_size`, `metadata_size`, `disks`, `disk_types`, `disk_remote_paths`, `tables`, `databases`, `functions`, `named_collections`, `rbac`
- Methods: `save_to_file()`, `load_from_file()`, `from_json_bytes()`, `to_json_bytes()`

#### PartInfo (`src/manifest.rs`)
- Fields: `name`, `size`, `backup_key`, `source`, `checksum_crc64`, `s3_objects`
- `source`: "uploaded" or "carried:{base_name}"
- `checksum_crc64`: CRC64/XZ of checksums.txt -- used for post-download verification

#### Config (`src/config.rs`)
- Relevant existing fields for Phase 2d:
  - `general.use_resumable_state: bool` (default true) -- already defined, not yet wired
  - `general.retries_on_failure: u32` (default 3)
  - `general.retries_pause: String` (default "5s")
  - `general.retries_jitter: u32` (default 30)
  - `backup.retries_on_failure: u32` (default 5)
  - `backup.retries_duration: String` (default "10s")
  - `backup.retries_jitter: f64` (default 0.1)
  - `clickhouse.check_parts_columns: bool` (default false) -- already defined, not yet wired
  - `clickhouse.skip_disks: Vec<String>` (default empty) -- already defined, not yet wired
  - `clickhouse.skip_disk_types: Vec<String>` (default empty) -- already defined, not yet wired
  - `clickhouse.secure: bool` (default false) -- already defined, not yet wired to ChClient
  - `clickhouse.skip_verify: bool` (default false) -- already defined
  - `clickhouse.tls_key: String` -- already defined
  - `clickhouse.tls_cert: String` -- already defined
  - `clickhouse.tls_ca: String` -- already defined

### Command Entry Points

#### `upload()` (`src/upload/mod.rs:161`)
- Signature: `pub async fn upload(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool, diff_from_remote: Option<&str>) -> Result<()>`
- Step 6: Currently uploads manifest with simple `s3.put_object()` -- needs atomic .tmp + CopyObject + delete
- No resume state tracking currently

#### `download()` (`src/download/mod.rs:73`)
- Signature: `pub async fn download(config: &Config, s3: &S3Client, backup_name: &str) -> Result<PathBuf>`
- No post-download CRC64 verification currently
- No resume state tracking currently

#### `restore()` (`src/restore/mod.rs:55`)
- Signature: `pub async fn restore(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool, data_only: bool) -> Result<()>`
- No resume state tracking currently
- No query to `system.parts` for already-attached parts

#### `backup::create()` (`src/backup/mod.rs:40`)
- Signature: `pub async fn create(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool, diff_from: Option<&str>) -> Result<BackupManifest>`
- Currently uses whole-table FREEZE; `--partitions` flag not wired

### ClickHouse Client Methods (`src/clickhouse/client.rs`)

#### Existing Methods (verified via LSP)
- `new(config: &ClickHouseConfig) -> Result<Self>` -- Line 57. Currently builds HTTP URL as `http://{host}:{port}`. TLS support needs `https://` and certificate config.
- `freeze_table(db, table, freeze_name) -> Result<()>` -- Line 141. Currently only whole-table FREEZE. Partition-level FREEZE needs new method or parameter.
- `list_tables() -> Result<Vec<TableRow>>` -- Line 171
- `get_disks() -> Result<Vec<DiskRow>>` -- Line 310
- `attach_part(db, table, part_name) -> Result<()>` -- Line 277
- `execute_ddl(ddl) -> Result<()>` -- Line 332

#### Missing Methods (needed for Phase 2d)
- `freeze_partition(db, table, partition, freeze_name) -> Result<()>` -- ALTER TABLE FREEZE PARTITION
- `query_parts(db, table) -> Result<Vec<PartRow>>` -- For resume: check which parts are already attached
- `check_parts_columns(targets) -> Result<Vec<...>>` -- Parts column consistency check (design 3.3)
- `query_disk_free_space() -> Result<Vec<DiskSpaceRow>>` -- For pre-flight space check (design 16.3)

#### DiskRow (`src/clickhouse/client.rs:44`)
- Fields: `name: String`, `path: String`, `disk_type: String`, `remote_path: String`
- `remote_path` added in Phase 2c (commit `b33a546`)

### S3Client Methods (`src/storage/s3.rs`)

#### Existing Methods (verified via LSP)
- `put_object(key, body) -> Result<()>` -- Line 160
- `put_object_with_options(key, body, content_type) -> Result<()>` -- Line 167
- `get_object(key) -> Result<Vec<u8>>` -- Line 223
- `copy_object(source_bucket, source_key, dest_key) -> Result<()>` -- Line 659
- `copy_object_with_retry(source_bucket, source_key, dest_key, allow_streaming) -> Result<()>` -- Line 782
- `delete_object(key) -> Result<()>` -- Line 365
- `head_object(key) -> Result<Option<u64>>` -- Line 427

#### For Manifest Atomicity
- Need to use existing `copy_object()` for .tmp -> final rename
- Need `delete_object()` to clean up .tmp

### List Module (`src/list.rs`)

#### BackupSummary
- Fields: `name`, `timestamp`, `size`, `compressed_size`, `table_count`, `is_broken`
- `is_broken: bool` already exists -- set when metadata.json is missing or unparseable
- `print_backup_table()` already prints `[BROKEN]` marker: `let status = if s.is_broken { " [BROKEN]" } else { "" };`
- Currently marks as broken but does NOT show the reason (design says: "metadata.json not found", "parse error")

### FreezeGuard (`src/backup/freeze.rs`)
- `freeze_table()` function: `pub async fn freeze_table(ch, guard, db, table, freeze_name, ignore_not_exists) -> Result<bool>`
- Currently only does whole-table FREEZE. Partition-level would need a new code path.

### Checksum Module (`src/backup/checksum.rs`)
- `compute_crc64(path) -> Result<u64>` and `compute_crc64_bytes(data) -> u64`
- Will be reused for post-download CRC64 verification

### Concurrency Module (`src/concurrency.rs`)
- `effective_upload_concurrency(config) -> u32`
- `effective_download_concurrency(config) -> u32`
- `effective_max_connections(config) -> u32`
- `effective_object_disk_copy_concurrency(config) -> u32`
- `effective_object_disk_server_side_copy_concurrency(config) -> u32`

### OwnedAttachParams (`src/restore/attach.rs:50`)
- All fields documented via LSP analysis (see document symbols above)
- Phase 2c already added S3 fields: `s3_client`, `disk_type_map`, `object_disk_server_side_copy_concurrency`, `allow_object_disk_streaming`, `disk_remote_paths`, `table_uuid`, `parts_by_disk`

## Cross-Reference: Callers of Modified Functions

### `upload()` callers
1. `main.rs:186` -- Upload command dispatch
2. `main.rs:327` -- CreateRemote command dispatch

### `download()` callers
1. `main.rs:214` -- Download command dispatch

### `restore()` callers
1. `main.rs:270` -- Restore command dispatch

### `backup::create()` callers
1. `main.rs:156` -- Create command dispatch
2. `main.rs:312` -- CreateRemote command dispatch

### `ChClient::new()` callers
1. `main.rs:154` -- Create command
2. `main.rs:268` -- Restore command
3. `main.rs:308` -- CreateRemote command

### `ChClient::freeze_table()` callers
1. `src/backup/freeze.rs:117` -- via `freeze_table()` public fn

### `list_local()` and `list_remote()` callers
1. `src/list.rs:50,61` -- via `list()` public fn

## Files That Will Be Modified

| File | Changes Needed |
|------|----------------|
| `src/upload/mod.rs` | Resume state tracking, manifest atomicity (.tmp + CopyObject + delete) |
| `src/download/mod.rs` | Resume state tracking, post-download CRC64 verification with retry |
| `src/restore/mod.rs` | Resume state tracking, system.parts query for already-attached |
| `src/backup/mod.rs` | Partition-level FREEZE routing, parts column check, disk filtering |
| `src/backup/freeze.rs` | Partition-level FREEZE PARTITION support |
| `src/clickhouse/client.rs` | TLS support in ChClient::new(), freeze_partition(), query_parts(), check_parts_columns(), query_disk_free_space() |
| `src/list.rs` | Broken backup reason display, clean_broken implementation |
| `src/main.rs` | Wire --resume, --partitions flags; implement clean_broken dispatch |
| `src/config.rs` | No new fields needed (all config fields already defined) |
| `src/error.rs` | Possibly add new error variants |
| `src/manifest.rs` | No changes needed |

## Files That Will Be Created

| File | Purpose |
|------|---------|
| (none expected) | All new logic fits in existing modules |

## Design Section References

| Feature | Design Section | Key Details |
|---------|---------------|-------------|
| Upload resume | 3.6 | `upload.state.json` in backup dir, `--resume` skips completed files, invalidate on param change |
| Download resume | 4 | `download.state.json`, same pattern as upload |
| Restore resume | 5.3 | `restore.state.json`, query system.parts for already-attached |
| State degradation | 16.1 | Write failure -> warning, never fatal |
| Post-download CRC64 | 4 | After decompress, compare CRC64 vs manifest; retry on mismatch |
| Manifest atomicity | 3.6 | Upload to .tmp, CopyObject to final, delete .tmp |
| Broken detection | 8.4 | Missing/corrupt metadata.json -> [BROKEN] in list; clean_broken command |
| Parts column check | 3.3 | Single batch query, skip Enum/Tuple/Nullable drift |
| Disk space pre-flight | 16.3 | system.disks free_space minus CRC64-matched hardlink savings |
| ClickHouse TLS | 12 | secure, skip_verify, tls_key, tls_cert, tls_ca |
| Partition-level backup | 3.4 | --partitions -> ALTER TABLE FREEZE PARTITION 'X' per partition |
| Disk filtering | 12 | skip_disks, skip_disk_types config |
