# Symbol and Reference Analysis

## Feature 1: `tables` Command

### CLI Definition
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/cli.rs` lines 260-273
- **Struct fields:** `tables: Option<String>`, `all: bool`, `remote_backup: Option<String>`
- Already fully defined in CLI, no changes needed to cli.rs

### Command Stub
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/main.rs` lines 382-384
- Current: `Command::Tables { .. } => { info!("tables: not implemented in Phase 1"); }`
- Needs: Full implementation dispatching to ChClient or S3 manifest

### Existing Query Methods (for live ClickHouse tables)
- **`ChClient::list_tables()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/clickhouse/client.rs` line 276
  - Returns `Vec<TableRow>` (database, name, engine, create_table_query, uuid, data_paths, total_bytes)
  - Already excludes system DBs by default
  - The `all` flag would need to query WITHOUT the system DB filter

### Remote Backup Table Listing
- **`BackupManifest::from_json_bytes()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/manifest.rs`
- **`S3Client::get_object()`** -- for fetching remote manifest
- Pattern: Same as `list_remote()` in `src/list.rs` line 144: `s3.get_object(&manifest_key).await`
- `manifest.tables` is `HashMap<String, TableManifest>` where key is `"db.table"`

### Table Filter
- **`table_filter::matches()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/table_filter.rs`
- Already used in backup::create() for `-t` flag filtering

---

## Feature 2: JSON/Object Column Detection

### Existing Column Check Pattern
- **`check_parts_columns()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/clickhouse/client.rs` line 604
  - Queries `system.parts_columns` for column type inconsistencies
  - Returns `Vec<ColumnInconsistency>` (database, table, column, types)
  - Called from `src/backup/mod.rs` lines 163-197

### Integration Point
- **`create()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/backup/mod.rs` line 73
  - After `list_tables()` and before FREEZE
  - Pattern: same pre-flight check pattern as `check_parts_columns`
  - Would add a new query method to ChClient, call it from `create()`

### ColumnInconsistency Row Type
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/clickhouse/client.rs` line 66
  ```rust
  pub struct ColumnInconsistency {
      pub database: String,
      pub table: String,
      pub column: String,
      pub types: String,
  }
  ```

### Callers of check_parts_columns
- `src/backup/mod.rs:169` -- `ch.check_parts_columns(&targets).await`
- `src/server/routes.rs:326` -- via `skip_check_parts_columns` param
- `src/watch/mod.rs:420` -- always false (let config control)

---

## Feature 3: Enhanced List Output

### BackupSummary Struct
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/list.rs` lines 26-42
  ```rust
  pub struct BackupSummary {
      pub name: String,
      pub timestamp: Option<DateTime<Utc>>,
      pub size: u64,              // uncompressed
      pub compressed_size: u64,   // ALREADY EXISTS but not displayed
      pub table_count: usize,
      pub is_broken: bool,
      pub broken_reason: Option<String>,
  }
  ```
- `compressed_size` is ALREADY populated from manifest in both `list_local` and `list_remote`

### print_backup_table (display function)
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/list.rs` lines 907-927
- Currently displays: name, status, timestamp, size (uncompressed), table_count
- Does NOT display: `compressed_size`, `data_format`
- Uses `format_size()` helper (line 887) for human-readable sizes

### Callers of print_backup_table
- `src/list.rs:61` -- for local backups
- `src/list.rs:72` -- for remote backups

### BackupManifest compressed_size
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/manifest.rs` line 44
  ```rust
  #[serde(default)]
  pub compressed_size: u64,
  ```
- Set in upload: `src/upload/mod.rs:726` -- `manifest.compressed_size = ...`

---

## Feature 4: Additional Compression Formats

### Current Compression Implementation

#### Upload Side
- **`compress_part()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/upload/stream.rs` line 16
  - Signature: `fn compress_part(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>>`
  - Uses `lz4_flex::frame::FrameEncoder` hardcoded
  - Called from `src/upload/mod.rs:481`

#### Download Side
- **`decompress_part()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/download/stream.rs` line 16
  - Signature: `fn decompress_part(data: &[u8], output_dir: &Path) -> Result<()>`
  - Uses `lz4_flex::frame::FrameDecoder` hardcoded
  - Called from `src/download/mod.rs:457`

- **`decompress_lz4()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/download/stream.rs` line 56
  - Raw LZ4 frame decompression utility

- **`compress_part()`** in download -- `/Users/rafael.siqueira/dev/personal/chbackup/src/download/stream.rs` line 36
  - Duplicate of upload's compress_part (used for testing roundtrips)

### S3 Key Format (hardcoded extension)
- **`s3_key_for_part()`** -- `/Users/rafael.siqueira/dev/personal/chbackup/src/upload/mod.rs` lines 68-76
  ```rust
  fn s3_key_for_part(backup_name: &str, db: &str, table: &str, part_name: &str) -> String {
      format!("{}/data/{}/{}/{}.tar.lz4", ...)
  }
  ```
- Extension `.tar.lz4` is hardcoded -- needs to become dynamic based on `data_format`

### All `.tar.lz4` References (production code, excluding tests)
- `src/upload/mod.rs:67-70` -- s3_key_for_part format string (PRIMARY)
- `src/manifest.rs:129` -- doc comment on backup_key field

### `.tar.lz4` References (test code, will need updates)
- `src/upload/stream.rs:100,105` -- test assertions
- `src/upload/mod.rs:983,992,1013,1022,1166,1272` -- test data
- `src/download/mod.rs:668,690,743,849,851,855` -- test data
- `src/list.rs:1484,1493,1508,1566,1567,1570,1611,1612,1619,1646,1651` -- test data
- `src/resume.rs:154,155,175` -- test data
- `src/manifest.rs:260,269,385,401` -- test data
- `src/backup/diff.rs:186,187,233,275,310,311,381,391,431,461,477` -- test data

### Config Validation
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/config.rs` lines 1235-1243
  ```rust
  match self.backup.compression.as_str() {
      "lz4" | "zstd" | "gzip" | "none" => {}
      other => { return Err(anyhow!("Unknown compression format: {}", other)) }
  }
  ```
- Already validates all 4 formats

### BackupConfig Fields
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/config.rs` line 336
  ```rust
  pub compression: String,       // default: "lz4"
  pub compression_level: i32,    // default: 1
  ```

### Manifest data_format Field
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/manifest.rs` line 40
  ```rust
  #[serde(default = "default_data_format")]
  pub data_format: String,
  ```
- Set from config in backup: `src/backup/mod.rs:529` -- `data_format: config.backup.compression.clone()`
- Set from config in upload: `src/upload/mod.rs:726` -- `manifest.data_format = data_format.clone()`
- Read during download: manifest is loaded, but `data_format` is NOT currently used to select decompressor

### File Extension Mapping (to be implemented)
| data_format | Archive Extension | Crate |
|-------------|------------------|-------|
| lz4 | .tar.lz4 | lz4_flex |
| zstd | .tar.zst | zstd |
| gzip | .tar.gz | flate2 |
| none | .tar | (none) |

### Dependencies Needed (Cargo.toml)
- Current: `lz4_flex = "0.11"`
- Need to add: `flate2` (gzip), `zstd` (zstd)

---

## Cross-Cutting Symbols

### table_filter Module
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/table_filter.rs`
- `matches(pattern, db, table)` -- glob matching for `-t` flag
- Used by tables command for filtering

### S3Client
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/storage/mod.rs`
- `get_object()`, `list_common_prefixes()` -- for remote backup table listing

### ChClient Construction in main.rs
- Tables command will need both `ChClient` and optionally `S3Client`
- Pattern follows other commands in main.rs (e.g., list at line 371-377)

---

## Document Symbol Analysis

### upload/stream.rs Symbols
- `compress_part(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>>` -- line 9

### download/stream.rs Symbols
- `decompress_part(data: &[u8], output_dir: &Path) -> Result<()>` -- line 8
- `compress_part(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>>` -- line 29
- `decompress_lz4(data: &[u8]) -> Result<Vec<u8>>` -- line 55

### list.rs Key Symbols
- `BackupSummary` struct -- line 26 (7 fields including compressed_size)
- `list()` -- line 46 (main entry point)
- `list_local()` -- line 80
- `list_remote()` -- line 124
- `print_backup_table()` -- line 907
- `format_size()` -- line 887
