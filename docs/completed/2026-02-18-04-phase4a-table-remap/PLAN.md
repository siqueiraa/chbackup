# Plan: Phase 4a -- Table / Database Remap

## Goal

Implement the `--as` flag (single table rename) and `-m` / `--database-mapping` flag (bulk database remap) for the `restore` command, including full DDL rewriting (table name, UUID removal, ZooKeeper path, Distributed engine references). Additionally, wire the `restore_remote` CLI command as a compound download+restore operation, and update all server route callers to pass remap parameters.

## Architecture Overview

The remap feature is entirely contained within the restore pipeline. No changes to backup, upload, or download modules.

```
CLI / Server API
  |
  v
main.rs / routes.rs  -- parse --as, -m flags, pass to restore()
  |
  v
restore::restore()  -- NEW params: rename_as, database_mapping
  |
  v
restore::remap (NEW module)
  |-- parse_database_mapping("prod:staging,logs:logs_copy") -> HashMap
  |-- RemapConfig::new(rename_as, db_mapping, config) -> Self
  |-- RemapConfig::remap_table_key(orig_key) -> (new_db, new_table)
  |-- rewrite_create_table_ddl(ddl, src_db, src_table, dst_db, dst_table, config) -> String
  |-- rewrite_create_database_ddl(ddl, src_db, dst_db) -> String
  |
  v
schema::create_databases()  -- uses remapped db names + rewritten DDL
schema::create_tables()     -- uses rewritten DDL
  |
  v
OwnedAttachParams { db: new_db, table: new_table, ... }
  |
  v
attach_parts_owned()  -- unchanged, receives remapped values
```

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **DDL strings**: Stored in `BackupManifest` (read-only during restore). Rewritten DDL is a *temporary copy* used only for schema creation.
- **Manifest table keys**: Format `"db.table"` in `BackupManifest.tables` HashMap. Remap translates these at restore time; manifest is never modified.
- **OwnedAttachParams.db / .table**: Receive the *destination* (remapped) values. No structural changes to the struct.
- **`create_databases` / `create_tables`**: Currently take manifest as source of truth. Will be modified to accept remap-aware inputs.

### What This Plan CANNOT Do
- No Mode A restore (`--rm` / DROP tables before restore) -- that is Phase 4d
- No `--partitions` flag support for restore -- deferred
- No RBAC/config backup restore -- Phase 4e
- No `ON CLUSTER` DDL rewriting (`restore_schema_on_cluster`) -- Phase 4d
- No `DatabaseReplicated` engine DDL rewriting -- future work

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| DDL regex fails on edge-case SQL syntax | YELLOW | Comprehensive unit tests with ReplicatedMergeTree, MergeTree, Distributed, MaterializedView DDL patterns |
| ZooKeeper path template not matching user config | GREEN | Use `config.clickhouse.default_replica_path` as template, with macro placeholders from design doc |
| `restore()` signature change breaks callers | GREEN | All 4 callers identified and listed in references.md; each updated explicitly |
| `restore_remote` CLI has different flag set than `restore` | GREEN | Verified in cli.rs: `restore_remote` has `--as`, `-m`, `--rm` but no `--partitions`, `--schema`, `--data-only` per design doc section 2 |
| Server `auto_resume` for restore passes None for remap params | GREEN | Auto-resume restores to original names (no remap), so `None` is correct |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Remap: .* -> .*` | yes (when `--as` used) | Logs table remap mapping |
| `Database remap: .* -> .*` | yes (when `-m` used) | Logs database remap mapping |
| `Rewriting DDL for remap` | yes (when remap active) | Confirms DDL rewriting occurred |
| `Starting restore` | yes | Existing log line from restore |
| `Restore complete` | yes | Existing log line from restore |
| `restore_remote: not implemented` | no (forbidden) | Old stub message must NOT appear |
| `--as flag is not yet implemented` | no (forbidden) | Old stub warning must NOT appear |
| `--database-mapping flag is not yet implemented` | no (forbidden) | Old stub warning must NOT appear |
| `database_mapping is not yet implemented` | no (forbidden) | Old stub warning from server route |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| `--rm` flag (Mode A DROP restore) | Separate design concern (destructive operation) | Phase 4d |
| `--partitions` for restore | Orthogonal feature, not related to remap | Future |
| `restore_schema_on_cluster` DDL rewriting | Requires cluster topology knowledge | Phase 4d |
| `DatabaseReplicated` engine remap | Rare engine, complex DDL | Future |
| RBAC/config restore | Separate domain | Phase 4e |

## Dependency Groups

```
Group A (Sequential - Core Remap):
  - Task 1: Create remap module with parsing and DDL rewriting
  - Task 2: Integrate remap into restore pipeline (modify restore(), create_databases(), create_tables())
  - Task 3: Wire CLI dispatch for restore and restore_remote commands

Group B (Independent of Task 3, depends on Task 2):
  - Task 4: Update server routes to pass remap parameters

Group C (Final - depends on all above):
  - Task 5: Update CLAUDE.md for modified modules
```

## Tasks

### Task 1: Create remap module with parsing and DDL rewriting

**Purpose:** Build `src/restore/remap.rs` with all remap logic as pure functions, fully testable without ClickHouse.

**TDD Steps:**

1. **Write failing tests** for `parse_database_mapping`:
   - `test_parse_database_mapping_single` -- `"prod:staging"` -> `{"prod": "staging"}`
   - `test_parse_database_mapping_multiple` -- `"prod:staging,logs:logs_copy"` -> two entries
   - `test_parse_database_mapping_empty` -- `""` -> empty HashMap
   - `test_parse_database_mapping_invalid` -- `"nocolon"` -> error

2. **Write failing tests** for `RemapConfig::new` and `remap_table_key`:
   - `test_remap_table_key_with_rename_as` -- `--as=dst_db.dst_table` with `-t src_db.src_table` -> `("dst_db", "dst_table")`
   - `test_remap_table_key_with_database_mapping` -- `prod.users` with `-m prod:staging` -> `("staging", "users")`
   - `test_remap_table_key_no_mapping` -- `prod.users` with no remap -> `("prod", "users")` (passthrough)
   - `test_remap_table_key_database_not_in_mapping` -- `logs.events` with `-m prod:staging` -> `("logs", "events")` (passthrough)

3. **Write failing tests** for `rewrite_create_table_ddl`:
   - `test_rewrite_ddl_simple_mergetree` -- Changes `CREATE TABLE src_db.src_table` to `CREATE TABLE dst_db.dst_table`
   - `test_rewrite_ddl_removes_uuid` -- `UUID 'abc-123'` is removed from DDL
   - `test_rewrite_ddl_replicated_zk_path` -- `ReplicatedMergeTree('/clickhouse/tables/{shard}/src_db/src_table', '{replica}')` -> rewrites ZK path to use `default_replica_path` template with dst_db/dst_table
   - `test_rewrite_ddl_distributed_table` -- `Distributed(cluster, src_db, src_table, rand())` -> updates db and table references
   - `test_rewrite_ddl_backtick_names` -- Handles backtick-quoted identifiers
   - `test_rewrite_ddl_preserves_rest` -- Column definitions, ORDER BY, etc. unchanged

4. **Write failing tests** for `rewrite_create_database_ddl`:
   - `test_rewrite_db_ddl` -- `CREATE DATABASE prod ENGINE = Atomic` -> `CREATE DATABASE staging ENGINE = Atomic`

5. **Implement** `src/restore/remap.rs`:

```rust
// src/restore/remap.rs

use std::collections::HashMap;
use anyhow::{bail, Result};
use tracing::info;

/// Parsed remap configuration from CLI flags.
#[derive(Debug, Clone)]
pub struct RemapConfig {
    /// Single table rename: (src_db, src_table) -> (dst_db, dst_table)
    pub rename_as: Option<(String, String, String, String)>,
    /// Database-level mapping: src_db -> dst_db
    pub database_mapping: HashMap<String, String>,
    /// ZK path template from config
    pub default_replica_path: String,
}

impl RemapConfig {
    pub fn new(
        rename_as_str: Option<&str>,
        table_pattern: Option<&str>,
        db_mapping_str: Option<&str>,
        default_replica_path: &str,
    ) -> Result<Option<Self>> { ... }

    pub fn is_active(&self) -> bool { ... }

    /// Given an original "db.table" key, return the destination (db, table).
    pub fn remap_table_key(&self, original_key: &str) -> (String, String) { ... }
}

/// Parse "-m prod:staging,logs:logs_copy" into HashMap.
pub fn parse_database_mapping(s: &str) -> Result<HashMap<String, String>> { ... }

/// Rewrite CREATE TABLE DDL for remap.
/// - Changes table name (db.table)
/// - Removes UUID clause
/// - Rewrites ZK path in ReplicatedMergeTree engine
/// - Updates Distributed engine references
pub fn rewrite_create_table_ddl(
    ddl: &str,
    src_db: &str,
    src_table: &str,
    dst_db: &str,
    dst_table: &str,
    default_replica_path: &str,
) -> String { ... }

/// Rewrite CREATE DATABASE DDL for remap.
pub fn rewrite_create_database_ddl(
    ddl: &str,
    src_db: &str,
    dst_db: &str,
) -> String { ... }
```

6. **Verify all tests pass**

**Files:**
- `src/restore/remap.rs` (CREATE)
- `src/restore/mod.rs` (MODIFY -- add `pub mod remap;`)

**Acceptance:** F001, F002

**Implementation Notes:**
- DDL table name replacement: regex `CREATE\s+(TABLE|VIEW|MATERIALIZED\s+VIEW|DICTIONARY)\s+(IF\s+NOT\s+EXISTS\s+)?` followed by backtick-optional db.table
- UUID removal: regex `UUID\s+'[0-9a-f-]+'` -> empty string (let ClickHouse assign new)
- ZK path rewriting: For `ReplicatedMergeTree('path', 'name')`, extract the first single-quoted param, replace with template from `default_replica_path` substituting `{database}` and `{table}` with dst values
- Distributed engine: regex `Distributed\s*\(\s*'?(\w+)'?\s*,\s*'?(\w+)'?\s*,\s*'?(\w+)'?` -> replace db and table args
- All functions are pure (no async, no I/O) for easy unit testing

---

### Task 2: Integrate remap into restore pipeline

**Purpose:** Modify `restore()`, `create_databases()`, and `create_tables()` to accept and use remap configuration.

**TDD Steps:**

1. **Write failing test** `test_remap_integration_table_keys`:
   - Given a manifest with keys `["prod.users", "prod.orders", "logs.events"]`
   - With database mapping `prod:staging`
   - Verify remap produces: `staging.users`, `staging.orders`, `logs.events`

2. **Modify `restore()` signature** -- add two new parameters:
```rust
pub async fn restore(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    data_only: bool,
    resume: bool,
    rename_as: Option<&str>,
    database_mapping: Option<&HashMap<String, String>>,
) -> Result<()>
```

3. **Inside `restore()`**, build `RemapConfig` and use it:
   - Build `RemapConfig` from params
   - When remapping is active, build a mapping: `original_key -> (new_db, new_table)`
   - In the database creation phase: create target databases (not source databases) with rewritten DDL
   - In the table creation phase: use rewritten DDL with target db.table
   - In the data attachment phase: use remapped db/table for `OwnedAttachParams`
   - In the resume state: use *original* table key for state file (since manifest parts reference original names) but query system.parts using *new* db/table

4. **Modify `create_databases()`** -- add remap parameter:
```rust
pub async fn create_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
    remap: Option<&RemapConfig>,
) -> Result<()>
```
   - When remap is active and database is in mapping: use `rewrite_create_database_ddl()` and create the *target* database
   - When remap is active: also collect target databases that don't exist in manifest (remapped databases)

5. **Modify `create_tables()`** -- add remap parameter:
```rust
pub async fn create_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    data_only: bool,
    remap: Option<&RemapConfig>,
) -> Result<()>
```
   - When remap is active: rewrite DDL before execution, check existence using target db/table

6. **Update all callers of `restore()`** (4 sites):
   - `src/main.rs:263` -- Restore CLI dispatch (will pass actual values in Task 3)
   - `src/server/routes.rs:558` -- restore_backup handler (will pass values in Task 4)
   - `src/server/routes.rs:775` -- restore_remote handler (will pass values in Task 4)
   - `src/server/state.rs:386` -- auto_resume (pass `None, None` -- auto-resume restores to original names)

7. **Update all callers of `create_databases()`** and `create_tables()`** (1 site each, both in `restore()`)

8. **Verify** compilation with `cargo check`

**Files:**
- `src/restore/mod.rs` (MODIFY)
- `src/restore/schema.rs` (MODIFY)
- `src/main.rs` (MODIFY -- temporary: pass `None, None` until Task 3)
- `src/server/routes.rs` (MODIFY -- temporary: pass `None, None` until Task 4)
- `src/server/state.rs` (MODIFY -- pass `None, None` permanently for auto-resume)

**Acceptance:** F003

**Implementation Notes:**
- The `already_attached` resume state tracks by *original* table key since that is what the state file contains
- `find_table_data_path()` and `find_table_uuid()` must use the *destination* (remapped) db/table since those are the live tables
- RC-015 check: `restore()` returns `Result<()>` -- no cross-task type mismatch
- RC-008 check: `RemapConfig` from Task 1 is used here -- correct sequencing

---

### Task 3: Wire CLI dispatch for restore and restore_remote

**Purpose:** Replace stub warnings with actual remap parameter passing for CLI `restore` and `restore_remote` commands.

**TDD Steps:**

1. **Remove** the `--as flag is not yet implemented` warning from `main.rs:234`
2. **Remove** the `--database-mapping flag is not yet implemented` warning from `main.rs:237`
3. **Parse `database_mapping`** string into HashMap at the CLI dispatch level:
```rust
let db_mapping = match &database_mapping {
    Some(s) => {
        let map = remap::parse_database_mapping(s)?;
        Some(map)
    }
    None => None,
};
```

4. **Pass to `restore()`**:
```rust
restore::restore(
    &config,
    &ch,
    &name,
    tables.as_deref(),
    schema,
    data_only,
    effective_resume,
    rename_as.as_deref(),
    db_mapping.as_ref(),
)
.await?;
```

5. **Implement `Command::RestoreRemote` dispatch** (replacing stub):
```rust
Command::RestoreRemote {
    tables,
    rename_as,
    database_mapping,
    rm,
    rbac,
    configs,
    named_collections,
    skip_empty_tables,
    resume,
    backup_name,
} => {
    // Warn about unimplemented flags
    if rm { warn!("--rm flag is not yet implemented, ignoring"); }
    if rbac { warn!("--rbac flag is not yet implemented, ignoring"); }
    if configs { warn!("--configs flag is not yet implemented, ignoring"); }
    if named_collections { warn!("--named-collections flag is not yet implemented, ignoring"); }
    if skip_empty_tables { warn!("--skip-empty-tables flag is not yet implemented, ignoring"); }

    let name = backup_name_required(backup_name, "restore_remote")?;
    let ch = ChClient::new(&config.clickhouse)?;
    let s3 = S3Client::new(&config.s3).await?;

    let db_mapping = match &database_mapping {
        Some(s) => Some(remap::parse_database_mapping(s)?),
        None => None,
    };

    // Step 1: Download from S3
    let effective_resume = resume && config.general.use_resumable_state;
    let _backup_dir = download::download(&config, &s3, &name, effective_resume).await?;

    // Step 2: Restore with remap
    restore::restore(
        &config,
        &ch,
        &name,
        tables.as_deref(),
        false, // schema_only (not a flag on restore_remote per design)
        false, // data_only (not a flag on restore_remote per design)
        effective_resume,
        rename_as.as_deref(),
        db_mapping.as_ref(),
    )
    .await?;

    info!(backup_name = %name, "RestoreRemote command complete");
}
```

6. **Verify** `cargo check` passes

**Files:**
- `src/main.rs` (MODIFY)

**Acceptance:** F004, F005

**Implementation Notes:**
- `restore_remote` follows `create_remote` compound command pattern (Task 1 in patterns.md)
- Per design doc section 2: `restore_remote` does NOT have `--schema`, `--data-only`, or `--partitions` flags. It DOES have `--as`, `-m`, `--rm`
- `backup_name_required()` already exists and is used by other commands
- RC-019 check: Pattern follows `Command::CreateRemote` exactly (download -> operation chaining)

---

### Task 4: Update server routes to pass remap parameters

**Purpose:** Add `rename_as` and `database_mapping` fields to server route request structs and pass them to `restore()`.

**TDD Steps:**

1. **Add `rename_as` field to `RestoreRequest`**:
```rust
pub struct RestoreRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    pub database_mapping: Option<String>,  // already exists
    pub rename_as: Option<String>,         // NEW
    pub rm: Option<bool>,
}
```

2. **Add `rename_as` and `database_mapping` fields to `RestoreRemoteRequest`**:
```rust
pub struct RestoreRemoteRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    pub rename_as: Option<String>,         // NEW
    pub database_mapping: Option<String>,  // NEW
}
```

3. **Update `restore_backup` handler** to parse and pass remap params:
   - Remove the `database_mapping is not yet implemented` warning
   - Parse `database_mapping` string into HashMap
   - Pass `rename_as` and parsed `database_mapping` to `restore()`

4. **Update `restore_remote` handler** to parse and pass remap params:
   - Parse `rename_as` and `database_mapping` from request
   - Pass to `restore()` call (Step 2 of the handler)

5. **Verify** `cargo check` passes

**Files:**
- `src/server/routes.rs` (MODIFY)

**Acceptance:** F006

**Implementation Notes:**
- `database_mapping` field already exists on `RestoreRequest` but was being ignored (warned as not implemented)
- `rename_as` field needs to be added to `RestoreRequest` (was missing)
- `RestoreRemoteRequest` needs both fields added
- Both use `#[serde(default)]` for backward compatibility (optional fields)
- RC-019: Follow existing server route pattern -- parse in spawned task, pass to function

---

### Task 5: Update CLAUDE.md for modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** `src/restore`, `src/server`

**TDD Steps:**

1. **Update `src/restore/CLAUDE.md`**:
   - Add `remap.rs` to Directory Structure
   - Add new Key Patterns section for DDL Rewriting / Remap:
     - `RemapConfig` struct and its role
     - `parse_database_mapping()` parsing convention
     - `rewrite_create_table_ddl()` transformations (name, UUID, ZK path, Distributed)
     - Updated `restore()` signature with new parameters
   - Update Public API section with new functions
   - Note: `restore()` signature now has 9 parameters

2. **Update `src/server/CLAUDE.md`**:
   - Update `RestoreRequest` struct documentation (add `rename_as` field)
   - Update `RestoreRemoteRequest` struct documentation (add `rename_as`, `database_mapping`)
   - Remove reference to "Phase 4a stub" for `database_mapping`

3. **Regenerate directory trees** using `tree` or `ls`

4. **Validate** all CLAUDE.md files have required sections: Parent Context, Directory Structure, Key Patterns, Parent Rules

**Files:**
- `src/restore/CLAUDE.md` (MODIFY)
- `src/server/CLAUDE.md` (MODIFY)

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All methods verified: `restore()`, `create_databases()`, `create_tables()`, `download()`, `execute_ddl()`, `database_exists()`, `table_exists()`, `parse_database_mapping()` (new) |
| RC-008 | PASS | Task 1 defines `RemapConfig` and `parse_database_mapping()` before Task 2 uses them. Task 2 modifies `restore()` before Task 3 wires CLI |
| RC-015 | PASS | `restore()` returns `Result<()>`, `download()` returns `Result<PathBuf>`. `restore_remote` chains: download -> restore. Types match |
| RC-016 | PASS | `RemapConfig` has all fields needed: `rename_as`, `database_mapping`, `default_replica_path`. Consumer tasks (2, 3, 4) use only these |
| RC-017 | PASS | No new `self.X` fields -- this is a library project, no actors |
| RC-019 | PASS | `restore_remote` CLI dispatch follows `create_remote` pattern exactly (download + operation chaining) |
| RC-021 | PASS | All file locations verified via Read/grep: `restore()` in `src/restore/mod.rs:57`, CLI in `src/cli.rs:121/219`, server routes in `src/server/routes.rs` |

## Notes

### Phase 4.5 Skip Justification
Phase 4.5 (Interface Skeleton Simulation) is skipped for this plan because:
- The only new module is `src/restore/remap.rs` which is a self-contained unit with no external dependencies beyond `std` and `anyhow`
- All existing symbols and their imports are verified in `context/knowledge_graph.json`
- The signature changes to `restore()`, `create_databases()`, `create_tables()` add `Option` parameters -- these are guaranteed to compile with `None` at unchanged call sites
- Comprehensive unit tests in Task 1 will catch any DDL regex issues before integration

### DDL Rewriting Strategy

The DDL rewriting uses regex-based string manipulation (not SQL parsing). This is consistent with the existing pattern in `ensure_if_not_exists_table()` and matches the Go tool's approach. Edge cases:

1. **Table name in DDL**: May appear with or without backticks: `CREATE TABLE db.table` or `CREATE TABLE \`db\`.\`table\``
2. **UUID clause**: Always appears as `UUID 'hex-hex-hex-hex-hex'` -- simple regex
3. **ZK path**: First single-quoted arg in `ReplicatedMergeTree(...)` -- need to handle nested parens
4. **Distributed args**: `Distributed(cluster, db, table, sharding_key)` -- positional args

### Validation: --as requires -t

Per design doc section 6.1: `--as` is used with `-t db.table`. If `--as` is provided without `-t`, or if `-t` matches multiple tables, the remap should fail with a clear error. This validation is in `RemapConfig::new()`.
