# Symbol and Reference Analysis

## Phase 1: MCP-Equivalent Symbol Analysis

### Key Symbols Being Modified

#### 1. `restore::restore()` (src/restore/mod.rs:62)
**Signature:**
```rust
pub async fn restore(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool, data_only: bool,
    resume: bool, rename_as: Option<&str>,
    database_mapping: Option<&HashMap<String, String>>,
) -> Result<()>
```
**Callers (4 total):**
- `src/main.rs:263` -- CLI `Command::Restore` dispatch
- `src/main.rs:385` -- CLI `Command::RestoreRemote` dispatch
- `src/server/routes.rs:572` -- `restore_backup()` API route
- `src/server/routes.rs:811` -- `restore_remote()` API route
- `src/server/state.rs:386` -- `auto_resume()` on server startup

**Impact:** Signature will NOT change. Internal flow restructuring only (table ordering, phased execution).

#### 2. `restore::schema::create_tables()` (src/restore/schema.rs:113)
**Signature:**
```rust
pub async fn create_tables(
    ch: &ChClient, manifest: &BackupManifest, table_keys: &[String],
    data_only: bool, remap: Option<&RemapConfig>,
) -> Result<()>
```
**Callers (1):**
- `src/restore/mod.rs:149` -- `restore()` function

**Impact:** Will be called multiple times with different table subsets (data tables, DDL-only tables). Signature stays the same.

#### 3. `TableManifest.dependencies` (src/manifest.rs:116)
**Type:** `Vec<String>`
**Serialization:** `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
**References (11 total across 4 files):**
- `src/manifest.rs:116` -- Field definition
- `src/manifest.rs:289,454,483,489` -- Tests
- `src/backup/mod.rs:249,431` -- Set to `Vec::new()` (metadata-only and data tables)
- `src/backup/diff.rs:123,330,351` -- Set to `Vec::new()` in diff tests
- `src/list.rs:1010,1537` -- Set to `Vec::new()` in list tests

**Impact:** Will be populated with actual dependency data from `system.tables` query during `backup::create()`.

#### 4. `TableManifest.metadata_only` (src/manifest.rs:112)
**Type:** `bool`
**References:** Used in restore/mod.rs:305 to skip data restore for DDL-only tables.

**Impact:** Will be used to classify tables into restore phases (data vs DDL-only).

#### 5. `TableManifest.engine` (src/manifest.rs:96)
**Type:** `String`
**References:** Used in restore/sort.rs:83 for `needs_sequential_attach()`.

**Impact:** Will be used for engine priority classification within restore phases.

#### 6. `is_metadata_only_engine()` (src/backup/mod.rs:570)
**Definition:**
```rust
fn is_metadata_only_engine(engine: &str) -> bool {
    matches!(engine,
        "View" | "MaterializedView" | "LiveView" | "WindowView" |
        "Dictionary" | "Null" | "Set" | "Join" | "Buffer" |
        "Distributed" | "Merge"
    )
}
```
**Callers (1):**
- `src/backup/mod.rs:227` -- `create()` during table classification

**Impact:** Currently private. The restore side will NOT call this function -- it uses `metadata_only` flag from manifest instead.

#### 7. `ChClient.list_tables()` (src/clickhouse/client.rs:250)
**Signature:** `pub async fn list_tables(&self) -> Result<Vec<TableRow>>`
**SQL:** Selects `database, name, engine, create_table_query, toString(uuid) as uuid, data_paths, total_bytes FROM system.tables`
**Callers:** Used in backup/mod.rs and restore/mod.rs

**Impact:** A new method `query_table_dependencies()` will be added alongside this. The existing `list_tables()` will NOT be modified.

### TableRow Type (src/clickhouse/client.rs:24-33)
```rust
pub struct TableRow {
    pub database: String,
    pub name: String,
    pub engine: String,
    pub create_table_query: String,
    pub uuid: String,
    pub data_paths: Vec<String>,
    pub total_bytes: Option<u64>,
}
```
**Note:** Does NOT currently include `dependencies_database` or `dependencies_table`. These will come from a separate query method, not by modifying `TableRow`.

## Phase 1.5: LSP Call Hierarchy Analysis

### `restore()` Outgoing Calls
The restore function currently calls (in order):
1. `BackupManifest::load_from_file()` -- Load manifest
2. `TableFilter::new()` -- Build filter
3. `RemapConfig::new()` -- Build remap config
4. `create_databases()` -- Phase 1
5. `create_tables()` -- ALL tables (data + DDL-only) in ONE pass
6. `detect_clickhouse_ownership()` -- For chown
7. `ch.list_tables()` -- For data paths
8. `S3Client::new()` -- For S3 disk parts
9. `ch.get_disks()` -- For S3 disk info
10. `tokio::spawn(attach_parts_owned)` -- Per-table data restore
11. `delete_state_file()` -- Cleanup resume state

**Key restructuring needed:** Step 5 (create_tables) currently processes ALL tables. Must be split into:
- Phase 2: Data tables only, sorted by engine priority
- Phase 3: DDL-only tables, topologically sorted by dependencies

### `create_tables()` Incoming Calls
Only 1 caller: `restore()`. Safe to restructure how it is called without cascading changes.

### `create_databases()` Incoming Calls
Only 1 caller: `restore()`. No impact.

## Cross-Module Data Flow for Dependencies

```
BACKUP TIME:
  ChClient.query_table_dependencies()  [NEW]
    -> HashMap<String, Vec<String>>  (table_key -> dependency_keys)

  backup::create()
    -> populates TableManifest.dependencies from query result
    -> serialized into metadata.json

RESTORE TIME:
  BackupManifest loaded from metadata.json
    -> TableManifest.dependencies available per table

  restore::restore()
    -> classify tables into phases using metadata_only + engine
    -> topological_sort() on DDL-only tables using dependencies
    -> create_tables(data_tables) [Phase 2]
    -> attach_parts() [Phase 2 data]
    -> create_ddl_objects(ddl_tables) [Phase 3, new function]
```

## Affected Files Summary

| File | Change Type | Description |
|------|-------------|-------------|
| `src/clickhouse/client.rs` | ADD method | `query_table_dependencies()` -- new query for system.tables deps |
| `src/backup/mod.rs` | MODIFY | Populate `dependencies` field during backup create |
| `src/restore/mod.rs` | MODIFY | Restructure flow into phased restore (2 -> 3) |
| `src/restore/topo.rs` | NEW file | Topological sort, engine priority, table classification |
| `src/restore/schema.rs` | ADD function | `create_ddl_objects()` with retry fallback |
| `src/lib.rs` | NO CHANGE | Module already declared |

## Key Anti-Patterns to Avoid (from preventive rules)

### RC-006: Verify all APIs exist
- `ChClient` does NOT have `query_table_dependencies()` yet -- must be added
- `create_ddl_objects()` does NOT exist yet -- must be added in restore/schema.rs
- `topological_sort()` does NOT exist yet -- must be added in restore/topo.rs

### RC-008: TDD sequencing
- `query_table_dependencies()` must be added BEFORE backup/mod.rs tries to use it
- `classify_restore_tables()` and `topological_sort()` must be added BEFORE restore/mod.rs uses them
- `create_ddl_objects()` must be added BEFORE restore/mod.rs calls it

### RC-019: Follow existing patterns
- New `query_table_dependencies()` must follow the `list_tables()` pattern exactly: SQL string, conditional logging, fetch_all, context wrap
- New `create_ddl_objects()` must follow `create_tables()` pattern: iterate keys, check exists, build DDL, execute

### RC-021: Verify struct locations
- `ChClient` is in `src/clickhouse/client.rs` (verified)
- `BackupManifest` is in `src/manifest.rs` (verified)
- `TableManifest` is in `src/manifest.rs` (verified)
- `restore()` is in `src/restore/mod.rs` (verified)
- `create_tables()` is in `src/restore/schema.rs` (verified)
