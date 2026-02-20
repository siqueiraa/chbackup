# Symbol & Reference Analysis — Phase 1 MVP

## Phase 1 Scope Verification

### Preventive Rules Applied

| Rule ID | Relevant? | How Applied |
|---------|-----------|-------------|
| RC-006 | YES | Every method/type in plan must be verified via grep or LSP before inclusion |
| RC-007 | YES | Tuple types (part name parsing) must verify field order |
| RC-008 | YES | TDD sequencing: fields must exist or be in preceding task |
| RC-015 | YES | Cross-task data flows: manifest structs must match between create/upload/download/restore |
| RC-016 | YES | Struct field completeness: BackupManifest must have all fields needed by all consumers |
| RC-017 | YES | State fields: all self.X in code must trace to declaration |
| RC-019 | YES | Follow existing patterns: ChClient/S3Client wrapper pattern for new methods |
| RC-021 | YES | File locations: verify actual struct locations before planning modifications |
| RC-032 | YES | Data authority: verified in data-authority.md |
| RC-001 | NO | No actor system in this project |
| RC-020 | NO | No Kameo messages in this project |

## Existing Symbols — Reference Map

### ChClient (src/clickhouse/client.rs:12)

**Current methods:** `new`, `ping`, `inner`
**Referenced from:**
- `src/main.rs:4` — import: `use chbackup::clickhouse::ChClient;`
- `src/main.rs:134` — usage: `let ch = ChClient::new(&config.clickhouse)?;`
- `src/clickhouse/mod.rs:3` — re-export: `pub use client::ChClient;`

**Phase 1 new methods needed (verified from design doc):**
- `freeze_table(db, table, freeze_name)` — `ALTER TABLE \`{db}\`.\`{table}\` FREEZE WITH NAME '{freeze_name}'`
- `unfreeze_table(db, table, freeze_name)` — `ALTER TABLE \`{db}\`.\`{table}\` UNFREEZE WITH NAME '{freeze_name}'`
- `list_tables(skip_tables, skip_engines)` — Query `system.tables`
- `get_table_ddl(db, table)` — Query `system.tables.create_table_query`
- `check_pending_mutations(targets)` — Query `system.mutations WHERE is_done = 0`
- `sync_replica(db, table)` — `SYSTEM SYNC REPLICA \`{db}\`.\`{table}\``
- `attach_part(db, table, part_name)` — `ALTER TABLE \`{db}\`.\`{table}\` ATTACH PART '{part_name}'`
- `get_version()` — `SELECT version()`
- `get_disks()` — Query `system.disks`
- `execute_ddl(ddl)` — Execute arbitrary DDL (CREATE TABLE, CREATE DATABASE)

**Pattern to follow (RC-019):**
```rust
// All new methods follow the same pattern as ping():
pub async fn method_name(&self, params...) -> Result<T> {
    let sql = format!("...", params);
    if self.log_sql_queries { info!(sql = %sql, "Executing"); }
    self.inner.query(&sql).execute().await.context("...")?;
    Ok(result)
}
```

**IMPORTANT:** ChClient needs access to `log_sql_queries` config flag. Currently ChClient stores only `host` and `port`. Need to add `log_sql_queries: bool` field.

### S3Client (src/storage/s3.rs:12)

**Current methods:** `new`, `ping`, `inner`, `bucket`, `prefix`
**Referenced from:**
- `src/main.rs:8` — import: `use chbackup::storage::S3Client;`
- `src/main.rs:141` — usage: `let s3 = S3Client::new(&config.s3).await?;`
- `src/storage/mod.rs:3` — re-export: `pub use s3::S3Client;`

**Phase 1 new methods needed:**
- `put_object(key, body: ByteStream)` — Upload single object to S3
- `get_object(key)` — Download object from S3 (returns streaming body)
- `list_prefixes(prefix)` — List common prefixes (backup names)
- `list_objects(prefix)` — List all objects under a prefix
- `delete_object(key)` — Delete single S3 object
- `delete_objects(keys: Vec<String>)` — Batch delete S3 objects
- `head_object(key)` — Check if object exists

**Pattern to follow (RC-019):**
```rust
// Follow same pattern as ping() - use self.inner directly
pub async fn put_object(&self, key: &str, body: ByteStream) -> Result<()> {
    self.inner.put_object()
        .bucket(&self.bucket)
        .key(key)
        .body(body)
        .send().await
        .context(format!("S3 put_object failed: {}", key))?;
    Ok(())
}
```

### ChBackupError (src/error.rs:5)

**Current variants:** ClickHouseError, S3Error, ConfigError, LockError, IoError
**Referenced from:**
- `src/lock.rs:7` — import: `use crate::error::ChBackupError;`
- `src/lock.rs:36,43,65` — usage in PidLock::acquire

**Phase 1 new variants needed:**
- `BackupError(String)` — Backup operation failures
- `RestoreError(String)` — Restore operation failures
- `ManifestError(String)` — Manifest parse/validation failures

### Config (src/config.rs:8)

**Fields used by Phase 1:**
- `config.clickhouse.data_path` — Shadow directory root: `{data_path}/shadow/`
- `config.clickhouse.sync_replicated_tables` — Whether to SYNC REPLICA before FREEZE
- `config.clickhouse.ignore_not_exists_error_during_freeze` — Skip tables dropped during backup
- `config.clickhouse.log_sql_queries` — Log SQL at info vs debug level
- `config.clickhouse.backup_mutations` — Whether to check/record pending mutations
- `config.clickhouse.skip_tables` — Glob patterns for tables to exclude
- `config.clickhouse.skip_table_engines` — Engine names to exclude
- `config.backup.tables` — Default table filter pattern (default: "*.*")
- `config.backup.allow_empty_backups` — Create backup even with no tables
- `config.backup.compression` — Compression algorithm (lz4, zstd, gzip, none)
- `config.backup.compression_level` — Compression level
- `config.s3.bucket` — S3 bucket name
- `config.s3.prefix` — S3 key prefix
- `config.s3.storage_class` — S3 storage class for uploads
- `config.s3.sse` — Server-side encryption type
- `config.s3.sse_kms_key_id` — KMS key ID for SSE

### Command enum (src/cli.rs:34)

**Phase 1 command variants and their fields:**

| Command | Fields Used by Phase 1 |
|---------|----------------------|
| `Create` | `tables: Option<String>`, `backup_name: Option<String>`, `schema: bool`, `rbac: bool`, `configs: bool`, `skip_check_parts_columns: bool` |
| `Upload` | `backup_name: Option<String>`, `delete_local: bool` |
| `Download` | `backup_name: Option<String>` |
| `Restore` | `tables: Option<String>`, `backup_name: Option<String>`, `schema: bool`, `data_only: bool`, `rm: bool` |
| `List` | `location: Option<Location>` |
| `Delete` | `location: Location`, `backup_name: Option<String>` |

**Fields NOT used in Phase 1 (deferred):**
- `Create.partitions` (Phase 2d)
- `Create.diff_from` (Phase 2b)
- `Create.resume` (Phase 2d)
- `Upload.diff_from_remote` (Phase 2b)
- `Upload.resume` (Phase 2d)
- `Download.hardlink_exists_files` (Phase 2)
- `Download.resume` (Phase 2d)
- `Restore.rename_as` (Phase 4a)
- `Restore.database_mapping` (Phase 4a)
- `Restore.partitions` (Phase 2d)
- `Restore.resume` (Phase 2d)

### main.rs Command Dispatch (src/main.rs:113-172)

All Phase 1 command match arms currently contain `info!("...: not implemented yet")` stubs. Phase 1 must:
1. Create ChClient and S3Client for commands that need them
2. Call into new module entry points
3. Pass Config, ChClient, S3Client references appropriately

**Current pattern (from List command):**
```rust
Command::List { location } => {
    let ch = ChClient::new(&config.clickhouse)?;
    let s3 = S3Client::new(&config.s3).await?;
    // ... call into list module
}
```

## New Symbols — Cross-Reference Map

### BackupManifest (planned: src/manifest.rs)

**Consumers (all must agree on field types):**
- `backup/mod.rs` — Creates manifest after FREEZE + collect
- `upload/mod.rs` — Reads manifest to find parts to upload; serializes manifest JSON to S3
- `download/mod.rs` — Deserializes manifest from S3; uses it to plan part downloads
- `restore/mod.rs` — Reads manifest for DDL, parts list, checksums
- `list.rs` — Reads manifest for display (name, timestamp, size, table count)

**RC-015 check (cross-task data flow):**
- backup::create() -> returns BackupManifest -> serialized to JSON on disk
- upload::upload() -> reads BackupManifest from JSON on disk -> uploads parts + manifest to S3
- download::download() -> reads BackupManifest from S3 -> downloads parts to local dir -> writes manifest JSON to disk
- restore::restore() -> reads BackupManifest from JSON on disk -> restores tables
- All 4 code paths use the same BackupManifest struct with serde Serialize+Deserialize

**RC-016 check (field completeness):**
All fields from design doc 7.1 must be present:

```rust
pub struct BackupManifest {
    pub manifest_version: u32,                        // Always 1
    pub name: String,                                 // Backup name
    pub timestamp: String,                            // ISO-8601
    pub clickhouse_version: String,                   // From SELECT version()
    pub chbackup_version: String,                     // From env!("CARGO_PKG_VERSION")
    pub data_format: String,                          // "lz4", "zstd", "gzip", "none"
    pub compressed_size: u64,                         // Sum of compressed part sizes
    pub metadata_size: u64,                           // Size of metadata files
    pub disks: HashMap<String, String>,               // disk_name -> path
    pub disk_types: HashMap<String, String>,           // disk_name -> type
    pub tables: HashMap<String, TableManifest>,        // "db.table" -> manifest
    pub databases: Vec<DatabaseInfo>,                  // Database DDL
    pub functions: Vec<serde_json::Value>,             // Phase 4
    pub named_collections: Vec<serde_json::Value>,     // Phase 4
    pub rbac: Option<RbacInfo>,                        // Phase 4
}
```

### TableFilter (planned: src/table_filter.rs)

**Consumers:**
- `backup/mod.rs` — Filter tables for FREEZE
- `restore/mod.rs` — Filter tables for restore
- `list.rs` — (maybe) filter display

**Pattern:** Simple glob matching on `database.table` strings using the `glob` crate's `Pattern::matches()`.

### Part Name Parsing

**RC-007 check (tuple field order):**
Part name format from design doc: `{partition}_{min_block}_{max_block}_{level}`
Example: `202401_1_50_3`

```
Split by '_' from the RIGHT (partition can contain underscores):
  - Level: last element (3)
  - MaxBlock: second-to-last (50)
  - MinBlock: third-to-last (1)
  - Partition: everything before that (202401)
```

Sort key for SortPartsByMinBlock: `(partition, min_block)` as `(String, u64)`.

## File Location Verification (RC-021)

| Struct | Assumed Location | Verified Location | Status |
|--------|-----------------|-------------------|--------|
| Config | src/config.rs | src/config.rs:8 | VERIFIED |
| ClickHouseConfig | src/config.rs | src/config.rs:98 | VERIFIED |
| S3Config | src/config.rs | src/config.rs:243 | VERIFIED |
| BackupConfig | src/config.rs | src/config.rs:326 | VERIFIED |
| ChClient | src/clickhouse/client.rs | src/clickhouse/client.rs:12 | VERIFIED |
| S3Client | src/storage/s3.rs | src/storage/s3.rs:12 | VERIFIED |
| ChBackupError | src/error.rs | src/error.rs:5 | VERIFIED |
| Command | src/cli.rs | src/cli.rs:34 | VERIFIED |
| PidLock | src/lock.rs | src/lock.rs:27 | VERIFIED |
| LockScope | src/lock.rs | src/lock.rs:101 | VERIFIED |

## Module Re-export Verification

| Module | mod.rs Location | Re-exports |
|--------|----------------|------------|
| clickhouse | src/clickhouse/mod.rs | `pub use client::ChClient;` |
| storage | src/storage/mod.rs | `pub use s3::S3Client;` |

## clickhouse Crate API Verification

### Row Derive for System Table Queries

The `clickhouse` crate v0.13 uses `#[derive(clickhouse::Row)]` with `serde::Deserialize` for typed queries. Verified from crate docs:

```rust
#[derive(clickhouse::Row, serde::Deserialize)]
struct TableRow {
    database: String,
    name: String,
    engine: String,
    create_table_query: String,
    uuid: String,
}

// Usage:
let rows = client.query("SELECT database, name, engine, create_table_query, uuid FROM system.tables WHERE ...")
    .fetch_all::<TableRow>().await?;
```

### Query Execution (DDL/DML without results)

```rust
client.query("ALTER TABLE `db`.`table` FREEZE WITH NAME 'name'")
    .execute().await?;
```

## aws-sdk-s3 API Verification

### PutObject with ByteStream

```rust
use aws_sdk_s3::primitives::ByteStream;

// From bytes:
let body = ByteStream::from(bytes_vec);
client.put_object().bucket("b").key("k").body(body).send().await?;

// From file path:
let body = ByteStream::from_path(path).await?;
```

### GetObject streaming body

```rust
let resp = client.get_object().bucket("b").key("k").send().await?;
let body = resp.body; // AggregatedBytes or streaming
let bytes = body.collect().await?.into_bytes();
```

### ListObjectsV2 with delimiter

```rust
let resp = client.list_objects_v2()
    .bucket("b")
    .prefix("prefix/")
    .delimiter("/")
    .send().await?;
// resp.common_prefixes() gives directory-like prefixes (backup names)
// resp.contents() gives individual objects
```

### DeleteObjects batch

```rust
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

let objects: Vec<ObjectIdentifier> = keys.iter()
    .map(|k| ObjectIdentifier::builder().key(k).build().unwrap())
    .collect();
let delete = Delete::builder().set_objects(Some(objects)).build().unwrap();
client.delete_objects().bucket("b").delete(delete).send().await?;
```

## Filesystem Path Conventions

### Local Backup Directory
- Root: `{config.clickhouse.data_path}/backup/{backup_name}/`
- Default data_path: `/var/lib/clickhouse`
- Example: `/var/lib/clickhouse/backup/daily_test/`

### Shadow Directory (FREEZE output)
- Root: `{config.clickhouse.data_path}/shadow/{freeze_name}/`
- Freeze name: `chbackup_{backup_name}_{sanitized_db}_{sanitized_table}`
- Example: `/var/lib/clickhouse/shadow/chbackup_daily_test_default_trades/`

### Local Backup Layout
```
{backup_dir}/
  metadata.json           # BackupManifest
  metadata/{db}/{table}.json  # Per-table metadata
  shadow/{db}/{table}/{part_name}/  # Hardlinked data parts
```

### S3 Key Layout
```
{prefix}/{backup_name}/
  metadata.json
  data/{url_encoded_db}/{url_encoded_table}/{part_name}.tar.lz4
```
