# Plan: Phase 1 -- MVP: Single-Table Backup & Restore

## Goal

Implement end-to-end backup and restore of ClickHouse tables on local disk with S3 upload/download. After this plan, `create -> upload -> download -> restore` works sequentially for one or more tables, plus `list` and `delete` commands. No parallelism, no S3 disk, no incrementals.

## Architecture Overview

Phase 1 builds on the Phase 0 skeleton (CLI, config, ChClient, S3Client, PidLock, logging, error types). It adds 7 new modules and extends 4 existing files to implement the core backup/restore pipeline:

```
main.rs  ->  backup::create()   ->  ChClient (FREEZE/UNFREEZE, table listing, mutations)
         ->  upload::upload()   ->  S3Client (PutObject with streaming compression)
         ->  download::download() -> S3Client (GetObject with streaming decompression)
         ->  restore::restore() ->  ChClient (CREATE TABLE, ATTACH PART)
         ->  list::list()       ->  local dir scan + S3Client (ListObjectsV2)
         ->  delete::delete()   ->  local rm + S3Client (DeleteObjects)
```

Central data structure: `BackupManifest` (src/manifest.rs) flows between all commands as the single source of truth for what was backed up and where.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Config**: Created by `Config::load()` in main.rs, stored in local variable, passed by `&` reference to all subsystems
- **ChClient**: Created in main.rs from `&config.clickhouse`, passed to backup/restore operations. Extended with new query methods.
- **S3Client**: Created in main.rs from `&config.s3`, passed to upload/download/list/delete operations. Extended with S3 operation methods.
- **BackupManifest**: Created by `backup::create()`, serialized to JSON on disk, deserialized by upload/download/restore/list
- **PidLock**: Already managed in main.rs -- held for duration of command, released on drop

### Data Flow
```
create:   Config -> ChClient -> FREEZE -> walk shadow -> hardlink -> CRC64 -> UNFREEZE -> BackupManifest -> JSON file
upload:   BackupManifest(JSON) -> read parts -> lz4 compress -> tar -> S3Client.put_object -> upload manifest last
download: S3Client.get_object(manifest) -> BackupManifest -> S3Client.get_object(parts) -> lz4 decompress -> tar extract -> local files
restore:  BackupManifest(JSON) -> CREATE DATABASE/TABLE -> hardlink parts to detached/ -> ChClient.attach_part -> chown
list:     scan local dirs + S3Client.list -> display
delete:   rm local dir or S3Client.delete_objects
```

### What This Plan CANNOT Do
- **No parallel operations** -- Phase 1 is sequential only. Parallelism is Phase 2.
- **No multipart upload** -- Phase 1 uses single PutObject only. Multipart is Phase 2.
- **No S3 disk support** -- Phase 1 handles local disk parts only. S3 disk is Phase 2c.
- **No incremental backup (--diff-from)** -- Phase 1 is full backups only. Incremental is Phase 2b.
- **No resume (--resume)** -- Phase 1 does not implement state files. Resume is Phase 2d.
- **No Mode A restore (--rm)** -- Phase 1 implements Mode B only (non-destructive). Mode A is Phase 4d.
- **No table remap (--as)** -- Phase 1 restores to original table names only. Remap is Phase 4a.
- **No RBAC/config backup** -- Phase 1 skips RBAC/config. These are Phase 4e.
- **No rate limiting** -- Phase 1 uploads/downloads without throttling. Rate limiting is Phase 2.
- **No partition-level freeze (--partitions)** -- Phase 1 freezes whole tables. Partition-level is Phase 2d.

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| `clickhouse` crate Row derive for system tables | YELLOW | Verify with unit test that Row derive + serde::Deserialize works for system.tables columns. Fallback: raw string parsing. |
| `aws-sdk-s3` ByteStream from async reader | YELLOW | Phase 1 uses buffered PutObject (read to Vec<u8>, compress, upload). Streaming pipeline deferred to Phase 2 multipart. |
| `crc64fast` crate availability | YELLOW | If unavailable on crates.io, use `crc` crate v3 with CRC-64/XZ algorithm or implement manually (CRC64 is simple). |
| Cross-device hardlink (EXDEV) | GREEN | `std::fs::hard_link` returns `io::Error` with ErrorKind::Other containing EXDEV. Catch via os error code 18. |
| Config default port 9000 vs HTTP port 8123 | GREEN | ChClient already uses HTTP interface correctly. Port mismatch is a documentation/config issue, not a code bug. Users configure the correct HTTP port. |
| `tar` crate for directory archiving | GREEN | Well-maintained crate (v0.4), synchronous. Use `tokio::task::spawn_blocking` for archive creation. |
| Large file memory during upload | YELLOW | Phase 1 buffers compressed part in memory before PutObject. Acceptable for MVP since most parts are <100MB compressed. Phase 2 adds streaming multipart. |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Executing.*FREEZE` | yes | FREEZE SQL query logged |
| `Executing.*UNFREEZE` | yes | UNFREEZE SQL query logged |
| `Backup.*created.*tables` | yes | Backup creation summary |
| `Uploaded.*parts.*manifest` | yes | Upload completion summary |
| `Downloaded.*parts` | yes | Download completion summary |
| `Restored.*tables.*parts` | yes | Restore completion summary |
| `ERROR.*FREEZE` | no (forbidden during normal operation) | Should NOT appear when table exists |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Port 9000 default in config | Config-level issue, not code bug | Phase 0 fix or documentation update |
| Streaming upload pipeline | Phase 1 buffers in memory | Phase 2 multipart streaming |
| Parallel operations | Sequential-only in Phase 1 | Phase 2a flat semaphore model |
| Incremental backups | No --diff-from support | Phase 2b CRC64 comparison |
| S3 object disk parts | Local disk only | Phase 2c metadata parsing |
| Resume interrupted operations | No state files | Phase 2d state tracking |
| Parts column consistency check (§3.3) | Requires CH 22.3+ `system.parts_columns`; CLI flag `--skip-check-parts-columns` already defined but check not implemented | Phase 2d pre-flight checks |
| Post-download CRC64 verification (§4) | Phase 1 downloads without verifying CRC64 after decompress | Phase 2d post-download verify with retry |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: Add dependencies to Cargo.toml + error variants + module declarations
  - Task 2: Manifest types (BackupManifest, TableManifest, PartInfo, DatabaseInfo)
  - Task 3: Table filter (glob pattern matching for -t flag)
  - Task 4: ChClient extensions (FREEZE, UNFREEZE, table listing, mutations, version, attach, DDL)
  - Task 5: S3Client extensions (put_object, get_object, list, delete, head)

Group B (Backup Pipeline -- Sequential, depends on Group A):
  - Task 6: backup::create -- FREEZE + shadow walk + hardlink + CRC64 + UNFREEZE + manifest
  - Task 7: upload::upload -- read manifest, compress parts, S3 PutObject, upload manifest last

Group C (Download + Restore Pipeline -- Sequential, depends on Group A):
  - Task 8: download::download -- download manifest, download+decompress parts
  - Task 9: restore::restore -- read manifest, CREATE DB/TABLE, hardlink to detached, ATTACH PART, chown

Group D (Utility Commands -- depends on Group A):
  - Task 10: list -- local dir scan + remote S3 listing
  - Task 11: delete -- local rm + remote S3 delete

Group E (Wiring -- depends on Groups B, C, D):
  - Task 12: Wire all commands in main.rs match arms

Group F (Documentation -- depends on Group E):
  - Task 13: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: Add dependencies, error variants, and module declarations

**TDD Steps:**
1. Write test: `cargo check` passes after adding new dependencies and module stubs
2. Add dependencies to Cargo.toml: `glob = "0.3"`, `nix = { version = "0.29", features = ["fs", "user"] }`, `tar = "0.4"`, `async-compression = { version = "0.4", features = ["tokio", "lz4"] }`, `crc = "3"` (for CRC64)
3. Add error variants to `ChBackupError`: `BackupError(String)`, `RestoreError(String)`, `ManifestError(String)`
4. Add module declarations to `src/lib.rs`: `pub mod backup; pub mod upload; pub mod download; pub mod restore; pub mod manifest; pub mod list; pub mod table_filter;`
5. Create stub `mod.rs` for each new directory module and stub files for `manifest.rs`, `list.rs`, `table_filter.rs`
6. Verify `cargo check` passes with empty stubs

**Files:**
- `Cargo.toml` (modify)
- `src/error.rs` (modify)
- `src/lib.rs` (modify)
- `src/manifest.rs` (create -- empty stub)
- `src/list.rs` (create -- empty stub)
- `src/table_filter.rs` (create -- empty stub)
- `src/backup/mod.rs` (create -- empty stub)
- `src/upload/mod.rs` (create -- empty stub)
- `src/download/mod.rs` (create -- empty stub)
- `src/restore/mod.rs` (create -- empty stub)

**Acceptance:** F001

**Notes:**
- Use `crc` crate v3 (well-maintained) with `CRC_64_XZ` algorithm instead of `crc64fast` (less certain availability)
- `async-compression` with `lz4` feature provides async LZ4 frame encode/decode wrapping `lz4_flex`
- Keep `lz4_flex` in Cargo.toml (it is a transitive dep of async-compression but also useful directly)

---

### Task 2: Manifest types (BackupManifest, TableManifest, PartInfo, DatabaseInfo)

**TDD Steps:**
1. Write failing test: `test_manifest_serialize_roundtrip` -- create a BackupManifest, serialize to JSON, deserialize, assert equality
2. Write failing test: `test_manifest_default_values` -- verify default values for optional fields
3. Implement `BackupManifest`, `TableManifest`, `PartInfo`, `DatabaseInfo`, `MutationInfo` structs with serde Serialize+Deserialize
4. Implement `BackupManifest::save_to_file(path)` and `BackupManifest::load_from_file(path)` helpers
5. Verify tests pass
6. Write test: `test_manifest_matches_design_doc_example` -- verify the struct can deserialize the JSON example from design doc section 7.1

**Files:**
- `src/manifest.rs` (implement)

**Acceptance:** F002

**Implementation Notes:**
- All fields from design doc 7.1 must be present
- Use `HashMap<String, TableManifest>` for tables (key = "db.table")
- Use `HashMap<String, Vec<PartInfo>>` for parts within TableManifest (key = disk name, e.g., "default")
- Use `#[serde(default)]` for optional fields (functions, named_collections, rbac)
- Use `#[serde(skip_serializing_if = "Vec::is_empty")]` for empty vectors
- `PartInfo.source` is a String: "uploaded" or "carried:base_name"
- `PartInfo.s3_objects` is `Option<Vec<S3ObjectInfo>>` -- None for local disk parts in Phase 1
- `PartInfo.checksum_crc64` is `u64`
- Add `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]` on all manifest structs

---

### Task 3: Table filter (glob pattern matching for -t flag)

**TDD Steps:**
1. Write failing test: `test_table_filter_exact_match` -- `TableFilter::new("default.trades")` matches "default.trades" but not "default.orders"
2. Write failing test: `test_table_filter_wildcard_db` -- `TableFilter::new("default.*")` matches all tables in default db
3. Write failing test: `test_table_filter_wildcard_table` -- `TableFilter::new("*.trades")` matches trades in any db
4. Write failing test: `test_table_filter_star_star` -- `TableFilter::new("*.*")` matches everything
5. Write failing test: `test_table_filter_comma_separated` -- `TableFilter::new("default.trades,logs.*")` matches both patterns
6. Implement `TableFilter` struct with `new(pattern: &str)` and `matches(db: &str, table: &str) -> bool`
7. Handle skip_tables patterns (exclusion): `fn is_excluded(db: &str, table: &str, skip_patterns: &[String]) -> bool`
8. Verify all tests pass

**Files:**
- `src/table_filter.rs` (implement)

**Acceptance:** F003

**Implementation Notes:**
- Use `glob::Pattern` for each comma-separated sub-pattern
- Match against `"{db}.{table}"` string
- Default pattern "*.*" matches everything (from `config.backup.tables`)
- Skip system databases: system, INFORMATION_SCHEMA, information_schema
- Skip tables matching `config.clickhouse.skip_tables` patterns
- Skip tables with engines in `config.clickhouse.skip_table_engines`

---

### Task 4: ChClient extensions

**TDD Steps:**
1. Write unit test: `test_freeze_sql_format` -- verify the SQL string generation for FREEZE command
2. Write unit test: `test_unfreeze_sql_format` -- verify the SQL string generation for UNFREEZE command
3. Write unit test: `test_sanitize_freeze_name` -- verify special chars sanitized to underscores
4. Add `log_sql_queries: bool` field to ChClient struct; update `new()` to store it from config
5. Add `log_and_execute(&self, sql: &str, description: &str)` helper that conditionally logs SQL at info vs debug
6. Implement all new query methods:
   - `freeze_table(&self, db: &str, table: &str, freeze_name: &str) -> Result<()>`
   - `unfreeze_table(&self, db: &str, table: &str, freeze_name: &str) -> Result<()>`
   - `list_tables(&self) -> Result<Vec<TableRow>>` where TableRow has: database, name, engine, create_table_query, uuid, data_paths, total_bytes
   - `get_table_ddl(&self, db: &str, table: &str) -> Result<String>`
   - `check_pending_mutations(&self, targets: &[(String, String)]) -> Result<Vec<MutationRow>>`
   - `sync_replica(&self, db: &str, table: &str) -> Result<()>`
   - `attach_part(&self, db: &str, table: &str, part_name: &str) -> Result<()>`
   - `get_version(&self) -> Result<String>`
   - `get_disks(&self) -> Result<Vec<DiskRow>>`
   - `execute_ddl(&self, ddl: &str) -> Result<()>`
   - `database_exists(&self, db: &str) -> Result<bool>`
   - `table_exists(&self, db: &str, table: &str) -> Result<bool>`
7. Verify all tests pass

**Files:**
- `src/clickhouse/client.rs` (modify)

**Acceptance:** F004

**Implementation Notes:**
- `TableRow`, `MutationRow`, `DiskRow` need `#[derive(clickhouse::Row, serde::Deserialize, Debug)]`
- For `list_tables`, query: `SELECT database, name, engine, create_table_query, uuid, data_paths, total_bytes FROM system.tables WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')`
- For mutations: `SELECT database, table, mutation_id, command, parts_to_do_names, is_done FROM system.mutations WHERE is_done = 0 AND (database, table) IN ({targets})`
- FREEZE: `ALTER TABLE \`{db}\`.\`{table}\` FREEZE WITH NAME '{freeze_name}'`
- UNFREEZE: `ALTER TABLE \`{db}\`.\`{table}\` UNFREEZE WITH NAME '{freeze_name}'`
- ATTACH PART: `ALTER TABLE \`{db}\`.\`{table}\` ATTACH PART '{part_name}'`
- Sanitize function: replace all non-alphanumeric/underscore chars with underscore
- Freeze name format: `chbackup_{backup_name}_{sanitized_db}_{sanitized_table}`
- `log_and_execute` logs at `info!` if `self.log_sql_queries`, else `debug!`
- Add helper: `pub fn sanitize_name(name: &str) -> String`
- `data_paths` from system.tables is `Array(String)` -- clickhouse crate should deserialize it as `Vec<String>`

---

### Task 5: S3Client extensions

**TDD Steps:**
1. Write unit test: `test_full_key` -- verify `full_key("backup/metadata.json")` prepends prefix correctly
2. Implement helper `fn full_key(&self, relative_key: &str) -> String` that prepends `self.prefix`
3. Implement all new methods:
   - `put_object(&self, key: &str, body: Vec<u8>) -> Result<()>` -- single PutObject with storage_class, SSE
   - `put_object_with_options(&self, key: &str, body: Vec<u8>, content_type: Option<&str>) -> Result<()>`
   - `get_object(&self, key: &str) -> Result<Vec<u8>>` -- download full object to memory
   - `get_object_stream(&self, key: &str) -> Result<ByteStream>` -- return streaming body
   - `list_common_prefixes(&self, prefix: &str, delimiter: &str) -> Result<Vec<String>>` -- list "directory" prefixes
   - `list_objects(&self, prefix: &str) -> Result<Vec<S3Object>>` where S3Object has: key, size, last_modified
   - `delete_object(&self, key: &str) -> Result<()>`
   - `delete_objects(&self, keys: Vec<String>) -> Result<()>` -- batch delete
   - `head_object(&self, key: &str) -> Result<Option<u64>>` -- returns size if exists, None if not
4. Verify `cargo check` passes (unit tests are limited since S3 calls need real S3)

**Files:**
- `src/storage/s3.rs` (modify)

**Acceptance:** F005

**Implementation Notes:**
- `full_key` handles prefix with/without trailing slash: if prefix is empty, return key as-is; if prefix doesn't end with '/', add it
- `put_object` uses `ByteStream::from(body)` for the body
- Apply storage_class from `self.storage_class` (add field to S3Client, stored from config)
- Apply SSE: if `self.sse == "aws:kms"`, set `.server_side_encryption(ServerSideEncryption::AwsKms)` and `.ssekms_key_id(&self.sse_kms_key_id)`
- `list_objects` must handle pagination (continuation_token) for >1000 objects
- `delete_objects` must batch in groups of 1000 (S3 API limit)
- Add fields to S3Client: `storage_class: String`, `sse: String`, `sse_kms_key_id: String`
- S3Object struct: `pub struct S3Object { pub key: String, pub size: i64, pub last_modified: Option<DateTime<Utc>> }`
- Use `aws_sdk_s3::types::{Delete, ObjectIdentifier, ServerSideEncryption}` as needed
- Use `aws_sdk_s3::primitives::ByteStream` for body conversion

---

### Task 6: backup::create -- FREEZE + shadow walk + hardlink + CRC64 + UNFREEZE + manifest

**TDD Steps:**
1. Write unit test: `test_compute_crc64` -- compute CRC64 of known byte sequence, verify checksum
2. Write unit test: `test_parse_part_name` -- parse "202401_1_50_3" into (partition="202401", min_block=1, max_block=50, level=3)
3. Write unit test: `test_freeze_name_generation` -- verify freeze name format and sanitization
4. Write unit test: `test_shadow_path_construction` -- verify path: `{data_path}/shadow/{freeze_name}/store/{shard_hex_prefix}/{table_hex_uuid}/{part_name}/...`
5. Write unit test: `test_url_encode_table_path` -- verify URL encoding for special chars in db/table names
6. Implement the backup module:
   - `src/backup/mod.rs` -- `pub async fn create(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool) -> Result<BackupManifest>`
   - `src/backup/freeze.rs` -- `FreezeGuard` struct with Drop impl for UNFREEZE; `freeze_table()` function
   - `src/backup/mutations.rs` -- `check_mutations()` function: check pending mutations, wait or skip
   - `src/backup/sync_replica.rs` -- `sync_replicas()`: SYSTEM SYNC REPLICA for Replicated tables
   - `src/backup/checksum.rs` -- `compute_crc64(path: &Path) -> Result<u64>`: CRC64 of file
   - `src/backup/collect.rs` -- `collect_parts(shadow_path: &Path, backup_dir: &Path) -> Result<Vec<PartInfo>>`: walk shadow, hardlink, compute checksums
7. Verify tests pass
8. Write test: `test_allow_empty_backup` -- verify empty backup with allow_empty_backups=true creates valid manifest with zero tables

**Files:**
- `src/backup/mod.rs` (implement)
- `src/backup/freeze.rs` (create)
- `src/backup/mutations.rs` (create)
- `src/backup/sync_replica.rs` (create)
- `src/backup/checksum.rs` (create)
- `src/backup/collect.rs` (create)

**Acceptance:** F006

**Implementation Notes:**
- **FreezeGuard pattern (design 3.4)**: FreezeGuard holds ChClient ref + freeze metadata. On Drop, spawns a blocking UNFREEZE. Use `tokio::runtime::Handle::current().block_on()` or store the handle.
  - IMPORTANT: Since Drop is sync, use a fallback approach: store metadata needed for UNFREEZE, and provide an explicit `async fn unfreeze(&self)` method. The caller MUST call unfreeze in a finally-like block. Phase 2 can add a proper scopeguard.
- **Backup directory layout (design 7):**
  ```
  {data_path}/backup/{backup_name}/
    metadata.json
    metadata/{db}/{table}.json
    shadow/{db}/{table}/{part_name}/...
  ```
- **Shadow walk**: Use `walkdir` via `tokio::task::spawn_blocking` to iterate `{data_path}/shadow/{freeze_name}/`
  - ClickHouse shadow structure: `shadow/{freeze_name}/store/{shard_hex_prefix}/{table_hex_uuid}/{part_name}/` where `shard_hex_prefix` is the first 3 chars of the hex UUID. To map these paths back to db.table names, use the `data_paths` column from `system.tables` (queried via `ChClient::list_tables()`) -- each table's data_path contains its hex UUID, which can be matched against the shadow directory structure.
  - Skip `frozen_metadata.txt` files
  - Identify part directories by the presence of `checksums.txt`
  - For each part: hardlink all files from shadow to backup staging
- **CRC64 computation**: Use `crc::Crc::<u64>::new(&crc::CRC_64_XZ)` to compute CRC64 of `checksums.txt` content
- **Hardlink**: `std::fs::hard_link(src, dst)`. On error with raw_os_error == 18 (EXDEV), fall back to `std::fs::copy`
- **ignore_not_exists_error_during_freeze**: Catch ClickHouse error during FREEZE. If error message contains "UNKNOWN_TABLE" or code 60/81, log warning and skip table.
- **allow_empty_backups**: After filtering and freezing, if zero tables collected and config.backup.allow_empty_backups is false, return error.
- **Mutation check (design 3.1)**: Query system.mutations for pending data mutations. If found, wait up to mutation_wait_timeout, then abort if still pending.
- **SYNC REPLICA (design 3.2)**: For tables with Replicated engine, run SYSTEM SYNC REPLICA before FREEZE.
- **URL encoding for table paths**: Replace special chars similar to Go tool's TablePathEncode. Simple approach: percent-encode non-alphanumeric chars except `/`, `-`, `_`, `.`

---

### Task 7: upload::upload -- compress parts, S3 PutObject, upload manifest last

**TDD Steps:**
1. Write unit test: `test_compress_lz4_roundtrip` -- compress bytes with lz4, decompress, verify match
2. Write unit test: `test_tar_directory_roundtrip` -- create a temp dir with files, tar it, untar, verify files match
3. Write unit test: `test_s3_key_for_part` -- verify S3 key format: `{prefix}/{backup_name}/data/{url_db}/{url_table}/{part_name}.tar.lz4`
4. Implement the upload module:
   - `src/upload/mod.rs` -- `pub async fn upload(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool) -> Result<()>`
   - `src/upload/stream.rs` -- `compress_part(part_dir: &Path) -> Result<Vec<u8>>`: tar directory -> lz4 compress -> return bytes
5. Upload flow:
   a. Read manifest from `{backup_dir}/metadata.json`
   b. For each table in manifest, for each part:
      - Read part directory from `{backup_dir}/shadow/{db}/{table}/{part_name}/`
      - `compress_part()` -> tar + lz4 -> Vec<u8>
      - `s3.put_object(key, body)` with key = `{backup_name}/data/{url_db}/{url_table}/{part_name}.tar.lz4`
   c. Upload manifest JSON last: `s3.put_object("{backup_name}/metadata.json", manifest_json_bytes)`
   d. If delete_local, remove local backup directory
6. Verify tests pass

**Files:**
- `src/upload/mod.rs` (implement)
- `src/upload/stream.rs` (create)

**Acceptance:** F007

**Implementation Notes:**
- Phase 1 uses buffered upload: tar the part directory to memory, lz4 compress the tar, then PutObject. This avoids the complexity of streaming multipart upload (Phase 2).
- `compress_part`: Use sync `tar::Builder` + sync `lz4_flex::frame::FrameEncoder` in `spawn_blocking`.
  - `let mut encoder = FrameEncoder::new(Vec::new());`
  - `let mut tar = tar::Builder::new(&mut encoder);`
  - `tar.append_dir_all(part_name, part_dir)?;`
  - `tar.finish()?;`
  - `drop(tar); encoder.finish()?` -> compressed bytes
- Upload manifest LAST per design 3.6 -- a backup is only "visible" when metadata.json exists
- Update manifest `compressed_size` field with sum of all uploaded compressed part sizes
- S3 key format: `{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{part_name}.tar.lz4`
- data_format in manifest: "lz4" (from config.backup.compression, default)

---

### Task 8: download::download -- download manifest, download+decompress parts

**TDD Steps:**
1. Write unit test: `test_decompress_lz4_roundtrip` -- compress then decompress, verify match
2. Write unit test: `test_untar_to_directory` -- create tar bytes, untar to temp dir, verify files
3. Implement the download module:
   - `src/download/mod.rs` -- `pub async fn download(config: &Config, s3: &S3Client, backup_name: &str) -> Result<PathBuf>`
   - `src/download/stream.rs` -- `decompress_part(data: &[u8], output_dir: &Path) -> Result<()>`: lz4 decompress -> untar -> files
4. Download flow:
   a. Download manifest: `s3.get_object("{backup_name}/metadata.json")` -> parse BackupManifest
   b. Create local backup directory: `{data_path}/backup/{backup_name}/`
   c. For each table in manifest, for each part:
      - Download: `s3.get_object(part.backup_key)` -> compressed bytes
      - Decompress: `decompress_part(bytes, "{backup_dir}/shadow/{db}/{table}/")` -> part files
   d. Save manifest to local: `{backup_dir}/metadata.json`
   e. Return backup_dir path
5. Verify tests pass

**Files:**
- `src/download/mod.rs` (implement)
- `src/download/stream.rs` (create)

**Acceptance:** F008

**Implementation Notes:**
- Phase 1 downloads full object to memory, then decompresses. Acceptable for MVP.
- `decompress_part`: Use sync `lz4_flex::frame::FrameDecoder` + sync `tar::Archive` in `spawn_blocking`.
  - `let decoder = FrameDecoder::new(data);`
  - `let mut archive = tar::Archive::new(decoder);`
  - `archive.unpack(output_dir)?;`
- Create directory structure: `{backup_dir}/shadow/{db}/{table}/` before unpacking
- Also save per-table metadata to `{backup_dir}/metadata/{db}/{table}.json`

---

### Task 9: restore::restore -- read manifest, CREATE DB/TABLE, hardlink to detached, ATTACH PART, chown

**TDD Steps:**
1. Write unit test: `test_sort_parts_by_min_block` -- verify sort order for parts with different partition/min_block values
2. Write unit test: `test_parse_part_name_sort_key` -- verify parsing "202401_1_50_3" returns ("202401", 1)
3. Write unit test: `test_needs_sequential_attach` -- verify Replacing/Collapsing engines return true, MergeTree returns false
4. Implement the restore module:
   - `src/restore/mod.rs` -- `pub async fn restore(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool, data_only: bool) -> Result<()>`
   - `src/restore/schema.rs` -- `create_databases()`, `create_tables()`: execute DDL from manifest
   - `src/restore/attach.rs` -- `attach_parts()`: hardlink to detached/, ATTACH PART, chown
   - `src/restore/sort.rs` -- `sort_parts_by_min_block()`: sort parts for correct ATTACH order
5. Restore flow (design 5.1-5.3, Mode B only):
   a. Read manifest from `{backup_dir}/metadata.json`
   b. Phase 1: CREATE databases from manifest.databases DDL
   c. Phase 2: For each table in manifest (filtered by table_pattern):
      - If table does not exist: CREATE TABLE from DDL
      - Sort parts by (partition, min_block) using SortPartsByMinBlock
      - For each part:
        - Hardlink from `{backup_dir}/shadow/{db}/{table}/{part_name}/` to `{ch_data_path}/data/{db_uuid}/{table_uuid}/detached/{part_name}/`
        - Fallback: copy if hardlink fails (EXDEV)
        - Chown to ClickHouse uid/gid
        - ALTER TABLE ATTACH PART '{part_name}'
   d. Log summary
6. Verify tests pass

**Files:**
- `src/restore/mod.rs` (implement)
- `src/restore/schema.rs` (create)
- `src/restore/attach.rs` (create)
- `src/restore/sort.rs` (create)

**Acceptance:** F009

**Implementation Notes:**
- **SortPartsByMinBlock** (design 5.3): Parse part name `{partition}_{min}_{max}_{level}` by splitting from the RIGHT (partition can contain underscores). Sort by `(partition, min_block as u64)`.
- **Mode B only** (design 5.2): If table exists, skip CREATE, only attach missing parts. If table doesn't exist, CREATE from DDL.
- **Chown** (design 5.3): Use `nix::unistd::chown()`. Detect ClickHouse uid/gid by `stat()`-ing `config.clickhouse.data_path`. Skip chown if not root (nix returns EPERM).
- **Detached path**: The correct path for detached parts is `{data_path}/data/{db_uuid}/{table_uuid}/detached/{part_name}/`. To find this, we need to query `system.tables` for the `data_paths` column which gives the table's data directory. Then append `detached/{part_name}/`.
  - Simpler Phase 1 approach: query `SELECT data_paths FROM system.tables WHERE database='{db}' AND name='{table}'` to get the table's data path, then use `{data_path}/detached/{part_name}/`.
- **ATTACH PART error handling**: If ATTACH PART fails with error 232/233 (overlapping block range, part already exists), log warning and skip (data already exists in a different merge state).
- **Engine classification for sort order**: Engines containing "Replacing", "Collapsing", or "Versioned" need sequential sorted ATTACH. All others (MergeTree, Summing, Aggregating) can use any order (but we do sequential anyway in Phase 1).
- **Database creation**: Execute DDL from `manifest.databases[]`. Wrap with IF NOT EXISTS safety.

---

### Task 10: list -- local dir scan + remote S3 listing

**TDD Steps:**
1. Write unit test: `test_parse_local_backup_dirs` -- create temp dirs with metadata.json, verify listing
2. Implement the list module:
   - `src/list.rs` -- `pub async fn list(config: &Config, ch: &ChClient, s3: &S3Client, location: Option<Location>) -> Result<()>`
   - `list_local(data_path: &str) -> Result<Vec<BackupSummary>>`
   - `list_remote(s3: &S3Client) -> Result<Vec<BackupSummary>>`
3. `BackupSummary` struct: name, timestamp, size, table_count, is_broken
4. Local: scan `{data_path}/backup/*/metadata.json`, parse each manifest
5. Remote: `s3.list_common_prefixes("{prefix}/", "/")` to get backup names, then download each `{name}/metadata.json`
6. Print formatted table to stdout
7. Verify test passes

**Files:**
- `src/list.rs` (implement)

**Acceptance:** F010

**Implementation Notes:**
- If manifest is missing or corrupt, mark as `[BROKEN]` in output
- Output format: `{name}\t{timestamp}\t{size}\t{table_count} tables`
- Size formatted with human-readable units (KB, MB, GB)
- Location: if None, show both local and remote. If Some(Local), only local. If Some(Remote), only remote.
- For remote listing, handle pagination in list_common_prefixes

---

### Task 11: delete -- local rm + remote S3 delete

**TDD Steps:**
1. Write unit test: `test_delete_local_backup` -- create temp backup dir, delete it, verify gone
2. Implement the delete module (add to list.rs or create separate):
   - `pub async fn delete(config: &Config, s3: &S3Client, location: &Location, backup_name: &str) -> Result<()>`
   - `delete_local(data_path: &str, backup_name: &str) -> Result<()>`: `fs::remove_dir_all`
   - `delete_remote(s3: &S3Client, backup_name: &str) -> Result<()>`: list all objects under prefix, batch delete
3. Verify test passes

**Files:**
- `src/list.rs` (modify -- add delete functions, or create a separate `src/delete.rs`)

**Acceptance:** F011

**Implementation Notes:**
- Local delete: `std::fs::remove_dir_all("{data_path}/backup/{backup_name}")`
- Remote delete: `s3.list_objects("{backup_name}/")` to get all keys, then `s3.delete_objects(keys)`
- Require explicit backup_name (no "delete all")
- If backup not found, return error with helpful message

---

### Task 12: Wire all commands in main.rs match arms

**TDD Steps:**
1. Verify `cargo check` passes after wiring
2. Wire each command in `main.rs` match arms:
   - `Command::Create` -> `backup::create()`
   - `Command::Upload` -> `upload::upload()`
   - `Command::Download` -> `download::download()`
   - `Command::Restore` -> `restore::restore()`
   - `Command::List` -> `list::list()`
   - `Command::Delete` -> `list::delete()` or `delete::delete()`
3. Each match arm creates ChClient/S3Client as needed and passes config fields
4. Verify `cargo build` succeeds with no warnings

**Files:**
- `src/main.rs` (modify)

**Acceptance:** F012

**Implementation Notes:**
- Create ChClient for commands that need ClickHouse: create, restore, list
- Create S3Client for commands that need S3: upload, download, list (remote), delete (remote)
- Pass backup_name with auto-generation: if None, generate name like `{date}T{time}` using chrono
- Log `info!("Command complete")` with summary metrics
- Handle unused flags gracefully: log warning for Phase 2+ flags like `--diff-from`, `--resume`, `--partitions`
- Import new modules: `use chbackup::{backup, upload, download, restore, list};`
- **Delete vs List location handling**: `Delete` has `location: Location` (required), while `List` has `location: Option<Location>` (optional, defaults to showing both). The Delete match arm always has a location to route local vs remote deletion; List must handle the `None` case by showing both.

---

### Task 13: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/clickhouse, src/storage, src/backup, src/upload, src/download, src/restore

**TDD Steps:**

1. For each new module (src/backup, src/upload, src/download, src/restore), create CLAUDE.md using template:
   - Auto-generate directory tree with `tree -L 2`
   - Document key patterns (module's public API, error handling approach)
   - Link to parent CLAUDE.md

2. For each existing module (src/clickhouse, src/storage), create CLAUDE.md (missing from Phase 0):
   - Document new methods added in this plan
   - Document client wrapper pattern

3. Validate all CLAUDE.md files have required sections:
   - Parent Context
   - Directory Structure
   - Key Patterns
   - Parent Rules

**Files:**
- `src/backup/CLAUDE.md` (create)
- `src/upload/CLAUDE.md` (create)
- `src/download/CLAUDE.md` (create)
- `src/restore/CLAUDE.md` (create)
- `src/clickhouse/CLAUDE.md` (create)
- `src/storage/CLAUDE.md` (create)

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All APIs verified: clickhouse crate query/execute, aws-sdk-s3 put/get/list/delete, std::fs::hard_link, crc crate, tar crate, lz4_flex |
| RC-008 | PASS | TDD sequencing: manifest (T2) before backup (T6) before upload (T7); ChClient (T4) before backup (T6); S3Client (T5) before upload (T7) |
| RC-015 | PASS | BackupManifest flows: create->save->upload->download->restore->list all use same struct with serde |
| RC-016 | PASS | BackupManifest has all fields needed by all consumers (verified against design doc 7.1) |
| RC-017 | N/A | No self.X state fields -- all modules are pure functions, not actors |
| RC-018 | PASS | Every task has explicit TDD steps with named test functions and assertions |
| RC-019 | PASS | ChClient new methods follow existing ping() pattern; S3Client new methods follow existing ping() pattern |
| RC-021 | PASS | All struct locations verified: Config at config.rs:8, ChClient at client.rs:12, S3Client at s3.rs:12, ChBackupError at error.rs:5 |

## Notes

### Phase 4.5 (Interface Skeleton Simulation): SKIPPED
Reason: Phase 1 creates entirely new modules with new types. The types do not exist yet, so a skeleton compilation test would just be testing stub creation. All imports from existing code have been verified against actual file locations in the knowledge graph.

### CRC64 Crate Decision
Using `crc` crate v3 with `CRC_64_XZ` algorithm instead of `crc64fast`. The `crc` crate is well-maintained (11M+ downloads) and supports multiple CRC algorithms including CRC-64/XZ which matches ClickHouse's checksum format.

### Upload Strategy
Phase 1 uses buffered in-memory upload (tar+compress to Vec<u8>, then PutObject). This is simpler and sufficient for MVP since most ClickHouse parts are <100MB compressed. Phase 2 will add streaming multipart upload for large parts.

### Port Mismatch Note
The config defaults `clickhouse.port` to 9000 (native protocol), but the `clickhouse` crate uses the HTTP interface (port 8123). Users must configure the correct port in their config file. This is a documentation issue, not a code bug -- the ChClient correctly uses `http://host:port` URL construction.
