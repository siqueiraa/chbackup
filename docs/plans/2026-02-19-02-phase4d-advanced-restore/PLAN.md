# Plan: Phase 4d -- Advanced Restore Modes

## Goal

Implement Mode A full restore (`--rm`), ATTACH TABLE mode, ZK conflict resolution, ON CLUSTER DDL, DatabaseReplicated detection, Distributed cluster rewrite, and pending mutation re-application -- completing the restore pipeline per design doc sections 5.1, 5.2, 5.3, and 5.7.

## Architecture Overview

Phase 4d adds six capabilities to the existing phased restore flow:

1. **Mode A (`--rm`)**: DROP tables/databases before CREATE, using reverse engine priority order with retry loop for dependency failures. New Phase 0 inserted before Phase 1.
2. **ON CLUSTER DDL**: When `restore_schema_on_cluster` is set, append `ON CLUSTER '{cluster}'` to all CREATE/DROP DDL statements. Skip for DatabaseReplicated databases.
3. **DatabaseReplicated detection**: Query `system.databases` for engine type; when `Replicated`, skip ON CLUSTER and regenerate UUIDs.
4. **ZK path conflict resolution**: Before creating Replicated tables, parse ZK path + replica from DDL, resolve macros via `system.macros`, check `system.zookeeper` for existing replicas, and DROP REPLICA if conflict.
5. **ATTACH TABLE mode** (`restore_as_attach: true`): For Replicated*MergeTree full restores, use DETACH TABLE SYNC -> DROP REPLICA -> ATTACH TABLE -> RESTORE REPLICA instead of part-by-part ATTACH.
6. **Distributed cluster rewrite** (`restore_distributed_cluster`): Rewrite the cluster name in Distributed engine DDL.
7. **Pending mutation re-apply** (design 5.7): After all parts attached for a table, re-apply mutations from `manifest.pending_mutations`.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **restore::restore()** in `mod.rs`: Orchestrates all phases. Owned by this module. We add a `rm: bool` parameter.
- **schema.rs**: Owns all schema DDL execution (CREATE/DROP databases and tables). We add `drop_tables()`, `drop_databases()`, and ON CLUSTER clause injection.
- **remap.rs**: Owns pure DDL string manipulation. We add `parse_replicated_params()`, `resolve_zk_macros()`, and extend `rewrite_distributed_engine()` for cluster name.
- **topo.rs**: Owns table classification and priority sorting. We add `reverse_drop_priority()` for Mode A DROP ordering.
- **attach.rs**: Owns part attachment. We add mutation re-apply after all parts attached.
- **ChClient** in `client.rs`: Owns all ClickHouse query execution. We add 7+ new methods.

### What This Plan CANNOT Do
- Cannot test ON CLUSTER or DatabaseReplicated without multi-node ClickHouse (deferred to integration tests)
- Cannot test ATTACH TABLE mode without ZooKeeper-backed Replicated tables (integration test only)
- Cannot test `system.zookeeper` queries without actual ZooKeeper
- Mode A + remap combination is exercised in unit tests only (no integration test for remap + --rm)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| `restore()` signature change (adding `rm: bool`) breaks 5 call sites | GREEN | All 5 sites identified in references.md; mechanical update |
| ON CLUSTER + DatabaseReplicated detection requires runtime CH query | GREEN | `query_database_engine()` is simple `SELECT engine FROM system.databases`; graceful fallback |
| ZK conflict resolution may fail if `system.zookeeper` is not queryable | YELLOW | Guard with `try/warn` -- if we cannot check ZK, proceed without DROP REPLICA (log warning) |
| ATTACH TABLE mode failure leaves table in DETACHED state | YELLOW | Wrap in try/catch: if ATTACH TABLE fails, attempt to re-CREATE from DDL as fallback |
| Mutation re-apply may take a long time for large mutations | GREEN | Log clear warnings per design 5.7; no timeout (user knows mutations may be slow) |
| DROP in wrong order causes dependency errors | GREEN | Reverse engine priority + retry loop (max 10 rounds, same pattern as `create_ddl_objects`) |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Phase 0: Dropping` | yes (Mode A) | Mode A DROP phase start |
| `Dropping table` | yes (Mode A) | Per-table DROP log |
| `Dropping database` | yes (Mode A) | Per-database DROP log |
| `ZK replica conflict detected` | yes (when ZK conflict) | ZK path already occupied |
| `SYSTEM DROP REPLICA` | yes (when ZK conflict) | Dropping conflicting replica |
| `ATTACH TABLE mode` | yes (when restore_as_attach) | ATTACH TABLE mode activation |
| `Re-applying.*mutation` | yes (when mutations) | Mutation re-apply start |
| `ON CLUSTER.*DDL` | yes (when on_cluster) | ON CLUSTER clause added |
| `DatabaseReplicated.*skipping ON CLUSTER` | yes (when db_replicated + on_cluster) | Skip ON CLUSTER for replicated DB |
| `Rewriting Distributed cluster` | yes (when dist_cluster set) | Cluster name rewrite |
| `ERROR:.*ATTACH TABLE failed` | no (forbidden in happy path) | Should NOT appear on success |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Mode A + --partitions | --partitions is not yet implemented for restore | Phase 4f |
| RBAC/config backup restore | Separate feature | Phase 4e |
| Table rename/remap (--as, -m) | Already implemented in Phase 4a | N/A |
| Parallel ATTACH within table | Deferred per CLAUDE.md | N/A |

## Dependency Groups

```
Group A (Sequential -- Foundation):
  - Task 1: ChClient new methods (all query/command methods for Mode A, ZK, ATTACH TABLE)
  - Task 2: DDL helpers in remap.rs (parse_replicated_params, resolve_zk_macros, distributed cluster rewrite, ON CLUSTER injection)
  - Task 3: Reverse DROP ordering in topo.rs

Group B (Sequential -- depends on Group A):
  - Task 4: Mode A DROP phase in schema.rs (drop_tables, drop_databases)
  - Task 5: ZK conflict resolution in schema.rs (check + DROP REPLICA before CREATE)
  - Task 6: ATTACH TABLE mode in mod.rs

Group C (Sequential -- depends on Group B):
  - Task 7: Mutation re-apply in attach.rs / mod.rs
  - Task 8: Wire rm parameter through restore() and all 5 call sites
  - Task 9: Integration of all features in restore() orchestrator

Group D (Independent -- Final):
  - Task 10: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: ChClient new methods

**Purpose:** Add all new ClickHouse query/command methods required by Mode A, ZK conflict resolution, ATTACH TABLE mode, and ON CLUSTER support.

**TDD Steps:**
1. Write unit test `test_drop_table_sql_generation` -- verify generated SQL for `drop_table()` (with and without ON CLUSTER)
2. Write unit test `test_drop_database_sql_generation` -- verify generated SQL for `drop_database()`
3. Write unit test `test_detach_table_sql_generation` -- verify SQL for `detach_table_sync()`
4. Write unit test `test_attach_table_sql_generation` -- verify SQL for `attach_table()`
5. Write unit test `test_restore_replica_sql_generation` -- verify SQL for `system_restore_replica()`
6. Write unit test `test_drop_replica_sql_generation` -- verify SQL for `drop_replica_from_zkpath()`
7. Write unit test `test_execute_mutation_sql_generation` -- verify SQL for `execute_mutation()`
8. Implement all methods following the existing `execute_ddl()` / `database_exists()` pattern
9. Verify `cargo check` passes with zero warnings

**New Methods (all follow existing `ChClient` patterns from client.rs):**

```rust
// -- DROP operations --

/// Drop a table (Mode A).
/// SQL: DROP TABLE IF EXISTS `db`.`table` [ON CLUSTER 'cluster'] SYNC
pub async fn drop_table(&self, db: &str, table: &str, on_cluster: Option<&str>) -> Result<()>

/// Drop a database (Mode A).
/// SQL: DROP DATABASE IF EXISTS `db` [ON CLUSTER 'cluster'] SYNC
pub async fn drop_database(&self, db: &str, on_cluster: Option<&str>) -> Result<()>

// -- ATTACH TABLE mode --

/// Detach a table synchronously.
/// SQL: DETACH TABLE `db`.`table` SYNC
pub async fn detach_table_sync(&self, db: &str, table: &str) -> Result<()>

/// Attach an entire table (not a part).
/// SQL: ATTACH TABLE `db`.`table`
pub async fn attach_table(&self, db: &str, table: &str) -> Result<()>

/// Restore replica metadata from local parts.
/// SQL: SYSTEM RESTORE REPLICA `db`.`table`
pub async fn system_restore_replica(&self, db: &str, table: &str) -> Result<()>

// -- ZK conflict resolution --

/// Drop a replica from ZooKeeper by explicit ZK path.
/// SQL: SYSTEM DROP REPLICA 'replica_name' FROM ZKPATH 'zk_path'
pub async fn drop_replica_from_zkpath(&self, replica_name: &str, zk_path: &str) -> Result<()>

/// Check if a replica exists at a given ZK path.
/// SQL: SELECT count() FROM system.zookeeper WHERE path='{zk_path}/replicas' AND name='{replica_name}'
/// Returns false on query error (system.zookeeper may not be accessible).
pub async fn check_zk_replica_exists(&self, zk_path: &str, replica_name: &str) -> Result<bool>

// -- DatabaseReplicated detection --

/// Query the engine of a database.
/// SQL: SELECT engine FROM system.databases WHERE name = '{db}'
/// Returns empty string if database not found.
pub async fn query_database_engine(&self, db: &str) -> Result<String>

// -- Mutation execution --

/// Execute a mutation command (ALTER TABLE ... DELETE/UPDATE WHERE ...).
/// The command is from MutationInfo.command (e.g., "DELETE WHERE user_id = 5").
/// SQL: ALTER TABLE `db`.`table` {command} SETTINGS mutations_sync=2
pub async fn execute_mutation(&self, db: &str, table: &str, command: &str) -> Result<()>
```

**Files:** `src/clickhouse/client.rs`
**Acceptance:** F001

**Implementation Notes:**
- Follow the existing pattern: `log_and_execute()` for DDL commands, `inner.query().fetch_one()` for queries
- `check_zk_replica_exists` should catch errors and return `Ok(false)` with a warning (system.zookeeper may be unavailable)
- `query_database_engine` returns empty string when database not found (not an error)
- ON CLUSTER parameter on `drop_table`/`drop_database` is `Option<&str>` -- `None` means no cluster clause
- SQL identifiers use backtick escaping per existing pattern
- `execute_mutation` uses `mutations_sync=2` per design 5.7 to wait for completion

---

### Task 2: DDL helpers in remap.rs

**Purpose:** Add pure functions for parsing Replicated engine parameters, resolving ZK macros, injecting ON CLUSTER clause, and rewriting Distributed cluster name.

**TDD Steps:**
1. Write test `test_parse_replicated_params_standard` -- parse `ReplicatedMergeTree('/path', 'replica')` -> `Some(("/path", "replica"))`
2. Write test `test_parse_replicated_params_replacing` -- parse `ReplicatedReplacingMergeTree('/path', 'replica', ver)` -> `Some(("/path", "replica"))`
3. Write test `test_parse_replicated_params_empty_parens` -- `ReplicatedMergeTree()` -> `None` (short syntax, uses server defaults)
4. Write test `test_parse_replicated_params_no_replicated` -- `MergeTree()` -> `None`
5. Write test `test_resolve_zk_macros` -- substitute `{database}`, `{table}`, `{shard}`, `{replica}`, `{uuid}` from HashMap
6. Write test `test_resolve_zk_macros_partial` -- some macros missing from map -> left as-is
7. Write test `test_add_on_cluster_clause_create_table` -- inject ON CLUSTER into `CREATE TABLE`
8. Write test `test_add_on_cluster_clause_drop_table` -- inject ON CLUSTER into `DROP TABLE`
9. Write test `test_add_on_cluster_clause_create_database` -- inject ON CLUSTER into `CREATE DATABASE`
10. Write test `test_rewrite_distributed_cluster` -- rewrite cluster name in `Distributed('old_cluster', db, table)` -> `Distributed('new_cluster', db, table)`
11. Write test `test_rewrite_distributed_cluster_no_distributed` -- non-Distributed DDL unchanged
12. Implement all functions
13. Verify `cargo check` passes

**New Functions:**

```rust
/// Parse the ZK path and replica name from a ReplicatedMergeTree DDL.
/// Returns None if not a Replicated engine or if using short syntax (empty parens).
pub fn parse_replicated_params(ddl: &str) -> Option<(String, String)>

/// Resolve macros in a ZK path template.
/// Substitutes {database}, {table}, {shard}, {replica}, {uuid} from the provided map.
/// Unknown macros are left as-is.
pub fn resolve_zk_macros(template: &str, macros: &HashMap<String, String>) -> String

/// Inject ON CLUSTER clause into a DDL statement.
/// Works for CREATE TABLE, CREATE DATABASE, DROP TABLE, DROP DATABASE.
/// Returns the DDL unchanged if ON CLUSTER is already present.
pub fn add_on_cluster_clause(ddl: &str, cluster: &str) -> String

/// Rewrite the cluster name in a Distributed engine DDL.
/// Changes Distributed('old_cluster', ...) to Distributed('new_cluster', ...).
/// Returns DDL unchanged if not a Distributed engine or cluster not found.
pub fn rewrite_distributed_cluster(ddl: &str, new_cluster: &str) -> String
```

**Files:** `src/restore/remap.rs`
**Acceptance:** F002

**Implementation Notes:**
- `parse_replicated_params` reuses the parsing logic from the existing private `rewrite_replicated_zk_path` -- factor out the shared position-finding code. It must extract the FIRST and SECOND single-quoted arguments after the Replicated engine `(`.
- `resolve_zk_macros` iterates the macros map and does `template.replace("{key}", value)` for each entry. Also handles `{database}` and `{table}` as special keys if not in the macros map.
- `add_on_cluster_clause` inserts ` ON CLUSTER '{cluster}'` after `CREATE TABLE|VIEW|DATABASE|DICTIONARY [IF NOT EXISTS] name` or `DROP TABLE|DATABASE [IF EXISTS] name`. Uses string find/insert, no regex.
- `rewrite_distributed_cluster` finds the `Distributed(` engine, then the first single-quoted argument (cluster), and replaces its value. This is an extension of the existing `rewrite_distributed_engine` parsing.
- All functions are pure (no async, no I/O) -- consistent with existing remap.rs pattern.

---

### Task 3: Reverse DROP ordering in topo.rs

**Purpose:** Add a function to produce the reverse engine priority ordering for Mode A DROP operations. Per design 5.1: "For DROP operations, order is reversed: Views/MVs dropped first, then inner tables, then regular tables."

**TDD Steps:**
1. Write test `test_reverse_drop_priority` -- Dictionary has highest priority (dropped last), Distributed/Merge have lowest (dropped first)
2. Write test `test_sort_tables_for_drop` -- given mixed table types, verify ordering: Distributed first, then Views, then Dictionaries, then data tables
3. Implement `engine_drop_priority()` and `sort_tables_for_drop()`
4. Verify `cargo check` passes

**New Functions:**

```rust
/// Engine priority for DROP operations (Mode A). Lower = dropped first.
/// Reverse of engine_restore_priority():
/// 0: Distributed, Merge (depend on nothing, safe to drop first)
/// 1: View, MaterializedView, LiveView, WindowView (depend on data tables)
/// 2: Dictionary (may be source for views)
/// 3: Regular data tables (MergeTree family -- dropped last)
pub fn engine_drop_priority(engine: &str) -> u8

/// Sort table keys for DROP ordering (reverse of restore priority).
/// Returns tables sorted by engine_drop_priority, with DDL-only objects first,
/// then data tables last.
pub fn sort_tables_for_drop(
    manifest: &BackupManifest,
    table_keys: &[String],
) -> Vec<String>
```

**Files:** `src/restore/topo.rs`
**Acceptance:** F003

**Implementation Notes:**
- `engine_drop_priority` is the inverse of `engine_restore_priority`:
  - Dictionary=2 (was 0), View/MV=1 (was 1), Distributed/Merge=0 (was 2), other(data)=3
- `sort_tables_for_drop` takes ALL tables (not just DDL-only) and sorts by engine_drop_priority. It does NOT use `classify_restore_tables` because DROP needs all tables in one sorted list.
- Design 5.1 also mentions retry loop for dependency failures, but that is handled in Task 4 (schema.rs `drop_tables`).

---

### Task 4: Mode A DROP phase in schema.rs

**Purpose:** Add `drop_tables()` and `drop_databases()` functions for Mode A. These execute DROP DDL in reverse engine priority order with retry loop for dependency failures.

**TDD Steps:**
1. Write unit test `test_drop_tables_sql_generation` -- verify SQL strings generated for various table types
2. Write unit test `test_drop_databases_skips_system` -- verify system databases are never dropped
3. Write unit test `test_drop_retry_logic` -- verify retry loop handles dependency failures
4. Implement `drop_tables()` following the `create_ddl_objects()` retry-loop pattern
5. Implement `drop_databases()`
6. Verify `cargo check` passes

**New Functions:**

```rust
/// Drop tables in reverse engine priority order (Mode A).
///
/// Tables are sorted by engine_drop_priority (Distributed/Merge first,
/// data tables last). Failures are retried in subsequent rounds (max 10),
/// following the same pattern as create_ddl_objects().
///
/// When `on_cluster` is set, DROP DDL includes ON CLUSTER clause.
/// When `is_database_replicated` callback returns true for a database,
/// ON CLUSTER is skipped for tables in that database.
pub async fn drop_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    remap: Option<&RemapConfig>,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
) -> Result<()>

/// Drop databases (Mode A).
///
/// Drops each database in the manifest that is not a system database
/// (system, information_schema, INFORMATION_SCHEMA).
/// When `on_cluster` is set, includes ON CLUSTER clause.
pub async fn drop_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
    remap: Option<&RemapConfig>,
    on_cluster: Option<&str>,
    replicated_databases: &HashSet<String>,
) -> Result<()>
```

**Files:** `src/restore/schema.rs`
**Acceptance:** F004

**Implementation Notes:**
- `drop_tables` uses `sort_tables_for_drop()` from Task 3 to get the correct DROP order
- Retry loop pattern copied from `create_ddl_objects()`: max 10 rounds, bail if zero progress after round > 0
- Uses `ch.drop_table(db, table, on_cluster_opt)` from Task 1 where `on_cluster_opt` is `None` if database is in `replicated_databases` set, otherwise `on_cluster` parameter
- System databases (`system`, `information_schema`, `INFORMATION_SCHEMA`) are never dropped
- When remap is active, drops destination tables (remapped names), not source names
- `drop_databases` drops in manifest order (no special ordering needed since tables are already dropped)

---

### Task 5: ZK conflict resolution in schema.rs

**Purpose:** Before creating Replicated tables, check ZooKeeper for existing replica paths and DROP REPLICA if conflict found. Also adds DatabaseReplicated detection.

**TDD Steps:**
1. Write unit test `test_resolve_zk_conflict_flow` -- verify the sequence: parse DDL -> resolve macros -> check ZK -> DROP REPLICA
2. Write unit test `test_is_replicated_engine` -- helper to detect Replicated* engines
3. Write unit test `test_skip_zk_check_for_non_replicated` -- non-Replicated tables skip ZK check
4. Implement `resolve_zk_conflict()` helper
5. Implement `detect_replicated_databases()` helper
6. Integrate ZK conflict resolution into table creation path
7. Verify `cargo check` passes

**New Functions:**

```rust
/// Check and resolve ZK replica path conflicts for a Replicated table.
///
/// 1. Parse ZK path + replica name from DDL
/// 2. Resolve macros using system.macros
/// 3. Check system.zookeeper for existing replica
/// 4. If conflict: SYSTEM DROP REPLICA
///
/// Returns Ok(()) on success or if not a Replicated table.
/// Logs warnings for conflicts and failures (non-fatal).
async fn resolve_zk_conflict(
    ch: &ChClient,
    ddl: &str,
    macros: &HashMap<String, String>,
    table_uuid: Option<&str>,
) -> Result<()>

/// Query which databases use the Replicated engine.
/// Returns a set of database names that should skip ON CLUSTER.
pub async fn detect_replicated_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
) -> HashSet<String>
```

**Files:** `src/restore/schema.rs`
**Acceptance:** F005

**Implementation Notes:**
- `resolve_zk_conflict` uses `parse_replicated_params()` from Task 2 and `ch.check_zk_replica_exists()` from Task 1
- Macros map includes `{database}`, `{table}` (from the destination names), plus whatever `ch.get_macros()` returns (typically `{shard}`, `{replica}`)
- If `{uuid}` is in the ZK path template, substitute from the DDL's UUID clause or `table_uuid` parameter
- ZK check failures are logged as warnings and do not abort restore (system.zookeeper may be unavailable or the table may not be on a ZK-backed server)
- `detect_replicated_databases` queries `ch.query_database_engine()` for each unique database in the manifest; caches results in a HashSet
- Integration: `resolve_zk_conflict` is called inside `create_tables()` path, BEFORE `ch.execute_ddl()`, only for tables with Replicated engine, only when creating new tables (not when table already exists)

---

### Task 6: ATTACH TABLE mode in mod.rs

**Purpose:** When `config.clickhouse.restore_as_attach` is true and the table uses a Replicated engine, use the DETACH/DROP REPLICA/ATTACH TABLE/RESTORE REPLICA flow instead of per-part ATTACH.

**TDD Steps:**
1. Write unit test `test_is_replicated_engine_detection` -- verify detection of all Replicated* engine variants
2. Write unit test `test_attach_table_mode_skips_non_replicated` -- non-Replicated tables use normal flow
3. Implement the ATTACH TABLE mode flow in `restore()` orchestrator
4. Verify `cargo check` passes

**Implementation Approach:**
In `restore()`, after `create_tables()` but before the per-table parallel attach loop, check `config.clickhouse.restore_as_attach`. For each data table that has a Replicated engine:
1. `ch.detach_table_sync(db, table)` -- detach the existing table
2. Parse ZK path + replica from DDL, resolve macros
3. `ch.drop_replica_from_zkpath(resolved_replica, resolved_path)` -- clean ZK state
4. Hard-link/copy parts to the table's data directory (NOT to detached/ since ATTACH TABLE reads from data dir)
5. `ch.attach_table(db, table)` -- re-attach the table (reads from its data directory)
6. `ch.system_restore_replica(db, table)` -- rebuild replica metadata from local parts

Non-Replicated tables continue to use the existing per-part ATTACH flow.

**Files:** `src/restore/mod.rs`
**Acceptance:** F006

**Implementation Notes:**
- ATTACH TABLE mode is an alternative to the per-part ATTACH loop, not a replacement. Both paths exist.
- The ATTACH TABLE flow hard-links parts to the table's main data directory (not `detached/`), because `ATTACH TABLE` reads directly from the data directory.
- If the ATTACH TABLE flow fails at any step, we log an error and fall back to the normal per-part ATTACH flow (defensive).
- This mode requires that `--rm` was also used (tables were freshly created) or the table was just created by this restore. If the table has existing data, ATTACH TABLE mode could lose data. Guard: only use ATTACH TABLE mode when `rm == true` OR the table was freshly created in this restore session.
- `is_replicated_engine(engine)` helper: checks if engine string starts with "Replicated" (covers ReplicatedMergeTree, ReplicatedReplacingMergeTree, etc.)

---

### Task 7: Mutation re-apply in mod.rs

**Purpose:** After all parts are attached for a table, re-apply pending mutations from the manifest per design 5.7.

**TDD Steps:**
1. Write unit test `test_mutation_reapply_format` -- verify that `MutationInfo.command` is correctly formatted into ALTER TABLE DDL
2. Write unit test `test_mutation_reapply_empty` -- no mutations -> no action
3. Implement mutation re-apply loop in the restore orchestrator
4. Verify `cargo check` passes

**Implementation:**

After the parallel table attach phase completes (after `try_join_all(handles)`), iterate over the restored tables and check each `TableManifest.pending_mutations`. For each non-empty list:

```
for each table with pending_mutations:
    WARN "table {db}.{table} backed up with N pending data mutations"
    for each mutation in pending_mutations:
        WARN "  mutation_id={}: {} ({} parts pending)"
        WARN "  Re-applying mutations... this may take time."
        ch.execute_mutation(db, table, &mutation.command)?
```

**Files:** `src/restore/mod.rs`
**Acceptance:** F007

**Implementation Notes:**
- Mutation re-apply runs AFTER all data is attached and BEFORE Phase 2b/3/4 (because mutations modify data in Phase 2 tables)
- Uses `ch.execute_mutation()` from Task 1 with `mutations_sync=2`
- Mutations are applied sequentially per-table (order matters for correctness)
- Failure is logged as a warning but does NOT abort restore (partial mutation re-apply is better than no data)
- The table key needs to be resolved through remap to get the destination db/table for the `ALTER TABLE` statement
- Only applies to data tables (DDL-only objects have no pending mutations)
- When `schema_only == true`, mutation re-apply is skipped

---

### Task 8: Wire rm parameter through restore() and all call sites

**Purpose:** Add `rm: bool` parameter to `restore()` function signature and update all 5 call sites.

**TDD Steps:**
1. Add `rm: bool` parameter to `restore()` after `data_only`
2. Update CLI Restore handler in `main.rs` -- pass `rm` instead of warning
3. Update CLI RestoreRemote handler in `main.rs` -- pass `rm` instead of warning
4. Update POST /api/v1/restore handler in `routes.rs` -- pass `req.rm.unwrap_or(false)`
5. Update POST /api/v1/restore_remote handler in `routes.rs` -- add `rm` to `RestoreRemoteRequest`, pass it
6. Update `auto_resume()` in `state.rs` -- pass `false` for `rm` (auto-resume never drops)
7. Remove the `--rm flag is not yet implemented` warnings from main.rs
8. Verify `cargo check` passes, `cargo test` passes

**Files:**
- `src/restore/mod.rs` (signature change)
- `src/main.rs` (2 call sites)
- `src/server/routes.rs` (2 call sites + add `rm` to `RestoreRemoteRequest`)
- `src/server/state.rs` (1 call site)

**Acceptance:** F008

**Implementation Notes:**
- The `rm` parameter is inserted after `data_only` in the parameter list: `..., data_only: bool, rm: bool, resume: bool, ...`
- `RestoreRemoteRequest` currently lacks an `rm` field (noted in references.md). Add `rm: Option<bool>` with `#[serde(default)]`.
- The "not yet implemented" warnings are REMOVED, replaced by passing the actual value.
- Auto-resume always passes `rm: false` -- resuming a restore should never DROP tables.

---

### Task 9: Integration of all features in restore() orchestrator

**Purpose:** Wire Mode A, ON CLUSTER, DatabaseReplicated, Distributed cluster rewrite, ZK conflict resolution, and ATTACH TABLE mode into the main `restore()` function.

**TDD Steps:**
1. Verify the new phase ordering: Phase 0 (DROP) -> Phase 1 (CREATE db) -> Phase 2 (CREATE + ATTACH) -> Phase 2.5 (mutations) -> Phase 2b (postponed) -> Phase 3 (DDL-only) -> Phase 4 (functions)
2. Implement the orchestration logic
3. Run `cargo check` and `cargo test` to verify compilation and existing tests pass
4. Verify no regressions in existing Mode B behavior

**Implementation Flow in `restore()`:**

```
// Derive ON CLUSTER config
let on_cluster = if config.clickhouse.restore_schema_on_cluster.is_empty() {
    None
} else {
    Some(config.clickhouse.restore_schema_on_cluster.as_str())
};

// Detect DatabaseReplicated databases
let replicated_databases = if on_cluster.is_some() {
    detect_replicated_databases(ch, &manifest).await
} else {
    HashSet::new()
};

// Get macros for ZK path resolution (needed for ZK conflict check)
let macros = ch.get_macros().await.unwrap_or_default();

// Distributed cluster rewrite config
let dist_cluster = &config.clickhouse.restore_distributed_cluster;

// Phase 0: DROP (Mode A only)
if rm && !data_only {
    let all_table_keys = ...; // all filtered tables
    drop_tables(ch, &manifest, &all_table_keys, remap_ref, on_cluster, &replicated_databases).await?;
    drop_databases(ch, &manifest, remap_ref, on_cluster, &replicated_databases).await?;
}

// Phase 1: CREATE databases (existing, now with ON CLUSTER support)
// Phase 2: CREATE tables (existing, now with ZK conflict check + ON CLUSTER)
// ...ATTACH... (existing or ATTACH TABLE mode)
// Phase 2.5: Mutation re-apply (new)
// Phase 2b, 3, 4: (existing)
```

**Files:** `src/restore/mod.rs`, `src/restore/schema.rs` (ON CLUSTER in create_databases/create_tables)
**Acceptance:** F009

**Implementation Notes:**
- ON CLUSTER clause injection: `create_databases` and `create_tables` need to receive `on_cluster: Option<&str>` and `replicated_databases: &HashSet<String>` parameters. Before executing DDL, if `on_cluster` is `Some` and the database is NOT in `replicated_databases`, call `add_on_cluster_clause(ddl, cluster)`.
- Distributed cluster rewrite: If `dist_cluster` is non-empty, apply `rewrite_distributed_cluster(ddl, dist_cluster)` during `create_tables()` DDL preparation (for Distributed engine tables).
- DatabaseReplicated UUID handling: When database is in `replicated_databases`, ensure UUID is stripped from DDL (already done by `remove_uuid_clause` in remap path; for non-remap path, add the same strip).
- `create_databases` signature changes: add `on_cluster: Option<&str>`, `replicated_databases: &HashSet<String>`
- `create_tables` signature changes: add `on_cluster: Option<&str>`, `replicated_databases: &HashSet<String>`, `macros: &HashMap<String, String>`, `dist_cluster: &str`
- All existing callers of `create_databases` and `create_tables` in `restore()` are updated to pass the new params. The `create_ddl_objects` and `create_functions` also need ON CLUSTER support.

---

### Task 10: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/restore, src/clickhouse

**TDD Steps:**

1. Read affected-modules.json for module list
2. For each module, regenerate directory tree
3. Detect and add new patterns:
   - src/restore/CLAUDE.md: Mode A DROP phase, ATTACH TABLE mode, ZK conflict resolution, ON CLUSTER, DatabaseReplicated, Distributed cluster rewrite, mutation re-apply, new public functions
   - src/clickhouse/CLAUDE.md: 9 new ChClient methods
4. Validate all CLAUDE.md files have required sections

**Files:** `src/restore/CLAUDE.md`, `src/clickhouse/CLAUDE.md`
**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 (Unverified APIs) | PASS | All existing methods verified via source reading; new methods clearly marked as "to add" in Task 1 |
| RC-008 (TDD sequencing) | PASS | Group A (Tasks 1-3) defines all new types/methods before Group B (Tasks 4-6) consumes them |
| RC-015 (Cross-task type mismatch) | PASS | `parse_replicated_params` returns `Option<(String, String)>` used consistently in Tasks 5 and 6 |
| RC-016 (Struct completeness) | PASS | No new structs; extending existing functions with new parameters |
| RC-017 (State field missing) | PASS | No new state fields; all state is passed as function parameters |
| RC-019 (Existing pattern) | PASS | All new code follows verified existing patterns (ChClient methods, schema.rs functions, remap.rs pure functions) |
| RC-021 (File location) | PASS | All file locations verified: ChClient in client.rs:14, RemapConfig in remap.rs:13, RestorePhases in topo.rs:75 |

## Notes

### Phase 4.5 Skip Justification
Phase 4.5 (Interface Skeleton Simulation) is skipped because:
- All changes are within existing files (no new crate imports needed)
- New functions follow exact patterns of existing code (same imports, same types)
- Type verification is thorough in context/symbols.md

### Signature Change Summary

| Function | Old Signature | New Parameters Added |
|----------|--------------|---------------------|
| `restore()` | 9 params | `rm: bool` (after `data_only`) |
| `create_databases()` | 3 params | `on_cluster: Option<&str>`, `replicated_dbs: &HashSet<String>` |
| `create_tables()` | 5 params | `on_cluster: Option<&str>`, `replicated_dbs: &HashSet<String>`, `macros: &HashMap<String, String>`, `dist_cluster: &str` |
| `create_ddl_objects()` | 4 params | `on_cluster: Option<&str>`, `replicated_dbs: &HashSet<String>` |
| `create_functions()` | 2 params | `on_cluster: Option<&str>` |
