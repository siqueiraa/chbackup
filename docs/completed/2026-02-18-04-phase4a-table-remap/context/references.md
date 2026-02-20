# Symbol and Reference Analysis

## Key Symbols for Phase 4a

### 1. `restore::restore()` -- Main Entry Point

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/mod.rs:57`

**Current Signature:**
```rust
pub async fn restore(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    data_only: bool,
    resume: bool,
) -> Result<()>
```

**All Callers (5 references across 4 files):**

| File | Line | Context |
|------|------|---------|
| `src/main.rs` | 263 | CLI `Command::Restore` dispatch |
| `src/server/routes.rs` | 558 | `POST /api/v1/restore/{name}` handler |
| `src/server/routes.rs` | 775 | `POST /api/v1/restore_remote/{name}` handler (Step 2) |
| `src/server/state.rs` | 386 | Auto-resume on restart |
| `src/restore/mod.rs` | 57 | Definition |

**Impact:** Adding `rename_as` and `database_mapping` parameters will require updating ALL 4 call sites.

---

### 2. `schema::create_tables()` -- Table DDL Creation

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/schema.rs:68`

**Current Signature:**
```rust
pub async fn create_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    data_only: bool,
) -> Result<()>
```

**All Callers (3 references across 2 files):**

| File | Line | Context |
|------|------|---------|
| `src/restore/mod.rs` | 40 | Import |
| `src/restore/mod.rs` | 127 | Called from `restore()` |
| `src/restore/schema.rs` | 68 | Definition |

**Impact:** Must be updated to accept remapped DDL or remap config. Only called from `restore()`.

---

### 3. `schema::create_databases()` -- Database DDL Creation

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/schema.rs:17`

**Current Signature:**
```rust
pub async fn create_databases(ch: &ChClient, manifest: &BackupManifest) -> Result<()>
```

**Impact:** Must handle database remap (create target databases for `-m` mapping).

---

### 4. `download::download()` -- Download Function

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/download/mod.rs:136`

**Current Signature:**
```rust
pub async fn download(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    resume: bool,
) -> Result<PathBuf>
```

**All Callers (5 references across 4 files):**

| File | Line | Context |
|------|------|---------|
| `src/main.rs` | 209 | CLI `Command::Download` dispatch |
| `src/server/routes.rs` | 482 | `POST /api/v1/download/{name}` handler |
| `src/server/routes.rs` | 753 | `POST /api/v1/restore_remote/{name}` handler (Step 1) |
| `src/server/state.rs` | 350 | Auto-resume on restart |
| `src/download/mod.rs` | 136 | Definition |

**Impact:** No changes to `download()` itself. `restore_remote` CLI dispatch will call it as-is.

---

### 5. `OwnedAttachParams` -- Part Attachment Parameters

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/attach.rs:59`

**Key Fields for Remap:**
```rust
pub struct OwnedAttachParams {
    pub db: String,           // line 63 - remapped destination database
    pub table: String,        // line 65 - remapped destination table
    pub table_uuid: Option<String>,  // line 89 - new UUID from CREATE
    pub engine: String,       // line 77 - engine name (unchanged)
    // ... other fields
}
```

**Impact:** No structural changes. The `db` and `table` fields will receive remapped values from the restore flow.

---

### 6. `BackupManifest` and `TableManifest`

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/manifest.rs`

**Key Types:**
- `BackupManifest.tables: HashMap<String, TableManifest>` (line 65) -- keys are `"db.table"`
- `BackupManifest.databases: Vec<DatabaseInfo>` (line 69)
- `TableManifest.ddl: String` (line 88)
- `TableManifest.uuid: Option<String>` (line 92)
- `TableManifest.engine: String` (line 96)
- `DatabaseInfo.name: String` (line 165)
- `DatabaseInfo.ddl: String` (line 168)

**Impact:** Read-only during restore. Remap transforms are applied before passing to schema creation and attach.

---

### 7. `ChClient` Methods Used in Remap

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/clickhouse/client.rs`

| Method | Line | Signature | Used For |
|--------|------|-----------|----------|
| `execute_ddl` | 441 | `async fn(&self, ddl: &str) -> Result<()>` | Execute rewritten DDL |
| `database_exists` | 482 | `async fn(&self, db: &str) -> Result<bool>` | Check target DB exists |
| `table_exists` | 504 | `async fn(&self, db: &str, table: &str) -> Result<bool>` | Check target table exists |
| `list_tables` | 250 | `async fn(&self) -> Result<Vec<TableRow>>` | Find live UUIDs for remap |
| `get_macros` | 409 | `async fn(&self) -> Result<HashMap<String, String>>` | Resolve ZK path macros |

---

### 8. Config Fields for DDL Rewriting

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/config.rs`

| Field | Line | Default | Purpose |
|-------|------|---------|---------|
| `clickhouse.default_replica_path` | 228 | `/clickhouse/tables/{shard}/{database}/{table}` | ZK path template for Replicated tables |
| `clickhouse.default_replica_name` | 231 | `{replica}` | Replica name template |
| `clickhouse.restore_distributed_cluster` | 162 | `""` (empty) | Rewrite Distributed engine cluster name |
| `clickhouse.restore_schema_on_cluster` | 160 | `""` (empty) | Add ON CLUSTER to DDL (Phase 4d) |

---

### 9. Server Route Types

**RestoreRequest** (line 604):
```rust
pub struct RestoreRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    pub database_mapping: Option<String>,  // already exists!
    pub rm: Option<bool>,
}
```
Note: `rename_as` field is MISSING from RestoreRequest -- needs to be added.

**RestoreRemoteRequest** (line 821):
```rust
pub struct RestoreRemoteRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    // Missing: rename_as, database_mapping, rm
}
```

---

### 10. `ensure_if_not_exists_table()` and `ensure_if_not_exists_database()`

**Location:** `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/schema.rs:119, 128`

These functions provide the reference pattern for DDL string manipulation. The remap DDL rewriting will follow a similar approach but with more complex parsing.

---

## Call Hierarchy Analysis

### `restore()` Incoming Calls
```
main.rs::Command::Restore  -->  restore::restore()
server::routes::restore_handler  -->  restore::restore()
server::routes::restore_remote_handler  -->  restore::restore()
server::state::auto_resume (restore case)  -->  restore::restore()
```

### `restore()` Outgoing Calls
```
restore::restore()  -->  BackupManifest::load_from_file()
                    -->  TableFilter::new()
                    -->  create_databases()
                    -->  create_tables()
                    -->  detect_clickhouse_ownership()
                    -->  ch.list_tables()
                    -->  S3Client::new()
                    -->  ch.get_disks()
                    -->  find_table_data_path()
                    -->  find_table_uuid()
                    -->  attach_parts_owned()  [via tokio::spawn]
```

### `create_tables()` Outgoing Calls
```
create_tables()  -->  ch.table_exists()
                 -->  ensure_if_not_exists_table()
                 -->  ch.execute_ddl()
```

## Symbols That Must NOT Be Modified

- `BackupManifest` struct (read-only during restore)
- `download::download()` function signature
- `OwnedAttachParams` struct fields (remap happens before construction)
- `attach_parts_owned()` function signature
- `cli.rs` Command enum definitions (flags already exist)
