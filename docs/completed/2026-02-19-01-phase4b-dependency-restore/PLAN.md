# Plan: Phase 4b -- Dependency-Aware Restore

## Goal

Implement topological sort for DDL-only objects (dictionaries, views, materialized views) during restore, populate the dependency graph in the manifest at backup time, and restructure `restore()` into the phased architecture specified in design doc section 5.1 / 5.5 / 5.6.

## Architecture Overview

The restore pipeline currently treats all tables identically -- data tables and DDL-only objects (views, dictionaries) are created in arbitrary HashMap iteration order, then data parts are attached. This causes dependency failures when views reference tables that haven't been created yet.

Phase 4b restructures the pipeline into explicit phases:

```
Phase 1: CREATE databases           (existing -- no change)
Phase 2: CREATE + ATTACH data tables (sorted by engine priority)
Phase 3: CREATE DDL-only objects     (topologically sorted by dependencies)
Phase 4: CREATE functions            (from manifest.functions)
```

The dependency graph is populated at backup time by querying `system.tables.dependencies_database` / `dependencies_table` (CH 23.3+). For older ClickHouse versions, the fallback is engine-priority sorting with a retry loop.

## Architecture Assumptions (VALIDATED)

### Component Ownership

- **BackupManifest**: Created by `backup::create()` (src/backup/mod.rs), serialized to JSON by `save_to_file()`, loaded by `restore::restore()` via `load_from_file()`
- **TableManifest.dependencies**: Field exists at src/manifest.rs:116, currently always `Vec::new()` -- will be populated from CH query
- **TableRow**: Defined at src/clickhouse/client.rs:25, returned by `list_tables()`, consumed by backup and restore -- NOT modified
- **ChClient**: Created in main.rs, cloned into backup/restore functions -- adding new query method
- **restore()**: Entry point at src/restore/mod.rs:62 -- signature unchanged, internal flow restructured

### What This Plan CANNOT Do

- Cannot change `restore()` function signature (5 callers: main.rs x2, server/routes.rs x2, server/state.rs x1)
- Cannot change `TableRow` struct fields (would break all callers of `list_tables()`)
- Cannot rely on CH dependency columns for CH < 23.3 (must have fallback retry loop)
- Cannot test dependency ordering without real ClickHouse (integration test only -- unit tests use mock data)
- Cannot change the manifest JSON schema in a breaking way (`dependencies` field already exists with `skip_serializing_if`)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| `system.tables` dependency columns missing on CH < 23.3 | YELLOW | `query_table_dependencies()` catches query error, returns empty HashMap; restore uses retry-loop fallback |
| Circular dependencies in DDL objects | YELLOW | `topological_sort()` detects cycles, breaks them by removing back-edges, logs warning |
| Breaking restore() callers | GREEN | Signature unchanged; all restructuring is internal to the function body |
| create_tables() called with different subsets | GREEN | Existing signature takes `table_keys: &[String]` -- already supports caller-controlled ordering |
| Manifest backward compatibility | GREEN | `dependencies` field uses `skip_serializing_if = "Vec::is_empty"` -- old manifests deserialize with empty deps |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Queried table dependencies` | yes | Backup: dependency query result from system.tables |
| `Populated dependencies for` | yes | Backup: per-table dependency population |
| `Classified .* tables:` | yes | Restore: table classification into phases |
| `Phase 2: .* data tables` | yes | Restore: data table creation phase |
| `Phase 3: .* DDL-only objects` | yes | Restore: DDL-only object creation phase |
| `Topological sort produced` | yes | Restore: topo sort result for DDL objects |
| `Created DDL object` | yes | Restore: per-object creation in Phase 3 |
| `DDL-only objects created` | yes | Restore: Phase 3 completion |
| `ERROR:.*dependency.*cycle` | no (forbidden) | Should NOT appear in normal operation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Phase 2b postponed tables (Kafka, S3Queue) | Separate feature (Phase 4c) | docs/roadmap.md Phase 4c |
| Mode A restore (`--rm` DROP) | Different feature (Phase 4d) | docs/roadmap.md Phase 4d |
| RBAC restore | Different feature (Phase 4e) | docs/roadmap.md Phase 4e |
| Named collections restore | Different feature (Phase 4e) | docs/roadmap.md Phase 4e |
| ON CLUSTER DDL propagation | Different feature (Phase 4d) | docs/roadmap.md Phase 4d |
| Named collections restore | Backup-side not yet populated (`manifest.named_collections` always empty until Phase 4e adds backup code); restore-side `create_named_collections()` would be dead code | docs/roadmap.md Phase 4e |

## Dependency Groups

```
Group A (Sequential -- Foundation):
  Task 1: Add query_table_dependencies() to ChClient
  Task 2: Populate dependencies in backup::create()

Group B (Sequential -- Restore Restructure):
  Task 3: Create restore/topo.rs (classification + topo sort + engine priority)
  Task 4: Add create_ddl_objects() to restore/schema.rs
  Task 6: Add create_functions() to restore/schema.rs
  Task 5: Restructure restore() for phased architecture (depends on Tasks 3, 4, 6)

Group C (Final -- Documentation):
  Task 7: Update CLAUDE.md for all modified modules
```

## Tasks

### Task 1: Add query_table_dependencies() to ChClient

**Purpose:** Add a method to query `system.tables` for dependency information. This is a separate query (not modifying `list_tables()`) to avoid breaking existing callers and because the dependency columns are `Array(String)` types that require different Row deserialization.

**TDD Steps:**

1. Write unit test `test_dependency_row_deserialize` -- verify DependencyRow struct can deserialize from expected column types.
2. Implement `DependencyRow` struct (private to client.rs) with fields: `database: String`, `name: String`, `dependencies_database: Vec<String>`, `dependencies_table: Vec<String>`.
3. Implement `pub async fn query_table_dependencies(&self) -> Result<HashMap<String, Vec<String>>>` on ChClient.
4. The method queries `system.tables` for dependency columns, combines parallel arrays into `"db.table"` format, and returns a map from `"db.table"` to `Vec<"dep_db.dep_table">`.
5. On query failure (CH < 23.3 where columns don't exist), catch the error, log a warning, and return `Ok(HashMap::new())`.
6. Verify test passes.

**Implementation Details:**

```rust
// Private struct -- only used by query_table_dependencies()
#[derive(clickhouse::Row, serde::Deserialize, Debug, Clone)]
struct DependencyRow {
    database: String,
    name: String,
    #[serde(rename = "dependencies_database")]
    dependencies_database: Vec<String>,
    #[serde(rename = "dependencies_table")]
    dependencies_table: Vec<String>,
}

// On ChClient:
pub async fn query_table_dependencies(&self) -> Result<HashMap<String, Vec<String>>> {
    let sql = "SELECT database, name, dependencies_database, dependencies_table \
               FROM system.tables \
               WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')";
    // ... follows list_tables() pattern: conditional logging, fetch_all, context wrap
    // Combine parallel arrays: dep_db[i] + "." + dep_table[i]
    // Filter out empty dependency entries
    // Return HashMap<"db.table", Vec<"dep_db.dep_table">>
}
```

**Pattern Reference:** Follow `list_tables()` at src/clickhouse/client.rs:250 exactly: SQL string, conditional `log_sql_queries`, `fetch_all::<DependencyRow>()`, `.context()`.

**Files:**
- `src/clickhouse/client.rs` -- Add `DependencyRow` struct + `query_table_dependencies()` method

**Acceptance:** F001

---

### Task 2: Populate dependencies in backup::create()

**Purpose:** During backup creation, call `query_table_dependencies()` and populate the `TableManifest.dependencies` field for every table (both metadata-only and data tables).

**TDD Steps:**

1. Write unit test `test_dependency_population_from_map` -- given a HashMap of dependencies and a list of table keys, verify dependencies are correctly populated in TableManifest structs.
2. Implement: after `ch.list_tables()` in `backup::create()`, call `ch.query_table_dependencies()` to get the dependency map.
3. For metadata-only tables (line 249): replace `dependencies: Vec::new()` with lookup from dependency map.
4. For data tables (line 431): replace `dependencies: Vec::new()` with lookup from dependency map.
5. Verify test passes.

**Implementation Details:**

The dependency map is queried once and stored as `let deps_map: HashMap<String, Vec<String>>`. For each table, `deps_map.get(&full_name).cloned().unwrap_or_default()` provides the dependencies.

```rust
// After list_tables() (around line 118), add:
let deps_map = ch.query_table_dependencies().await.unwrap_or_else(|e| {
    warn!(error = %e, "Failed to query table dependencies (CH < 23.3?), dependencies will be empty");
    HashMap::new()
});
info!(
    tables_with_deps = deps_map.values().filter(|v| !v.is_empty()).count(),
    "Queried table dependencies"
);

// At line 249 (metadata-only tables):
dependencies: deps_map.get(&full_name).cloned().unwrap_or_default(),

// At line 431 (data tables inside tokio::spawn):
// The deps_map must be cloned into the spawn closure
dependencies: deps_clone.get(&full_name).cloned().unwrap_or_default(),
```

**Note on tokio::spawn:** The `deps_map` must be wrapped in `Arc` or cloned before the spawn loop, since it needs to be moved into each spawned task. Following the existing pattern where `all_tables_arc = Arc::new(all_tables.clone())`, use `let deps_arc = Arc::new(deps_map)` and clone the Arc into each task.

**Files:**
- `src/backup/mod.rs` -- Add dependency query call and populate `dependencies` fields

**Acceptance:** F002

---

### Task 3: Create restore/topo.rs (classification + topo sort + engine priority)

**Purpose:** New module containing the core logic for phased restore: table classification into restore phases, topological sort for DDL-only objects, and engine priority ordering.

**TDD Steps:**

1. Write unit test `test_engine_restore_priority` -- verify engine priority values match design doc 5.1.
2. Write unit test `test_classify_restore_tables_basic` -- given a manifest with data tables and DDL-only tables, verify correct classification into `RestorePhases`.
3. Write unit test `test_topological_sort_simple` -- given tables with dependencies, verify correct ordering.
4. Write unit test `test_topological_sort_cycle_detection` -- given circular dependencies, verify the sort completes with a warning (not an error).
5. Write unit test `test_topological_sort_empty_deps` -- given tables with no dependencies, verify engine-priority sorting is used as fallback.
6. Write unit test `test_classify_with_inner_tables` -- verify `.inner` tables (MV storage targets) get priority 1 within Phase 2.
7. Implement `engine_restore_priority()`, `classify_restore_tables()`, `topological_sort()`.
8. Verify all tests pass.

**Implementation Details:**

```rust
// src/restore/topo.rs

use std::collections::{HashMap, HashSet, VecDeque};
use anyhow::Result;
use tracing::{info, warn};
use crate::manifest::{BackupManifest, TableManifest};

/// Engine priority for Phase 2 (data tables). Lower = created first.
/// 0: Regular MergeTree tables
/// 1: .inner tables (MV storage targets -- name starts with ".inner" or ".inner_id")
pub fn data_table_priority(table_key: &str) -> u8 {
    // Use split_once('.') to correctly handle .inner tables whose names contain dots
    // e.g. "default..inner_id.5f3a7b2c-..." -> table_name = ".inner_id.5f3a7b2c-..."
    let table_name = table_key
        .split_once('.')
        .map(|(_, t)| t)
        .unwrap_or(table_key);
    if table_name.starts_with(".inner") {
        1
    } else {
        0
    }
}

/// Engine priority for Phase 3 (DDL-only objects). Lower = created first.
/// Per design doc 5.1:
/// 0: Dictionary
/// 1: View, MaterializedView, LiveView, WindowView
/// 2: Distributed, Merge
pub fn engine_restore_priority(engine: &str) -> u8 {
    match engine {
        "Dictionary" => 0,
        "View" | "MaterializedView" | "LiveView" | "WindowView" => 1,
        "Distributed" | "Merge" => 2,
        _ => 3, // Null, Set, Join, Buffer -- rarely restored as DDL-only
    }
}

/// Classification of tables into restore phases.
#[derive(Debug, Clone)]
pub struct RestorePhases {
    /// Phase 2: Data tables (MergeTree family) sorted by engine priority.
    pub data_tables: Vec<String>,
    /// Phase 2b: Postponed tables (streaming engines) -- empty for now (Phase 4c).
    pub postponed_tables: Vec<String>,
    /// Phase 3: DDL-only objects, topologically sorted by dependencies.
    pub ddl_only_tables: Vec<String>,
}

/// Classify filtered tables into restore phases using metadata_only flag.
pub fn classify_restore_tables(
    manifest: &BackupManifest,
    table_keys: &[String],
) -> RestorePhases {
    let mut data_tables: Vec<String> = Vec::new();
    let mut ddl_only_tables: Vec<String> = Vec::new();

    for key in table_keys {
        if let Some(tm) = manifest.tables.get(key) {
            if tm.metadata_only {
                ddl_only_tables.push(key.clone());
            } else {
                data_tables.push(key.clone());
            }
        }
    }

    // Sort data tables by priority (regular first, then .inner tables)
    data_tables.sort_by_key(|k| data_table_priority(k));

    info!(
        data = data_tables.len(),
        ddl_only = ddl_only_tables.len(),
        "Classified {} tables: {} data, {} DDL-only",
        table_keys.len(),
        data_tables.len(),
        ddl_only_tables.len(),
    );

    RestorePhases {
        data_tables,
        postponed_tables: Vec::new(), // Phase 4c
        ddl_only_tables,
    }
}

/// Topological sort of DDL-only tables using their dependency graph (Kahn's algorithm).
///
/// If dependencies are empty (CH < 23.3 or old manifest), falls back to
/// engine-priority sorting.
///
/// Handles cycles by breaking them (removes back-edges) with a warning log.
pub fn topological_sort(
    tables: &HashMap<String, TableManifest>,
    keys: &[String],
) -> Result<Vec<String>> {
    // Check if any table has non-empty dependencies
    let has_deps = keys.iter().any(|k| {
        tables
            .get(k)
            .map_or(false, |tm| !tm.dependencies.is_empty())
    });

    if !has_deps {
        // Fallback: sort by engine priority only
        let mut sorted = keys.to_vec();
        sorted.sort_by_key(|k| {
            tables
                .get(k)
                .map_or(3, |tm| engine_restore_priority(&tm.engine))
        });
        info!(count = sorted.len(), "Topological sort produced (engine-priority fallback, no dependency info)");
        return Ok(sorted);
    }

    // Build adjacency and in-degree for Kahn's algorithm
    let key_set: HashSet<&String> = keys.iter().collect();
    let mut in_degree: HashMap<&String, usize> = HashMap::new();
    let mut adjacency: HashMap<&String, Vec<&String>> = HashMap::new();

    for key in keys {
        in_degree.entry(key).or_insert(0);
        adjacency.entry(key).or_default();
    }

    // For each table, its dependencies are tables that must be created BEFORE it.
    // So if table A depends on table B, there is an edge B -> A (B must come first).
    for key in keys {
        if let Some(tm) = tables.get(key) {
            for dep in &tm.dependencies {
                // Only count edges within our key set (deps on Phase 2 tables are already satisfied)
                if key_set.contains(dep) {
                    adjacency.entry(dep).or_default().push(key);
                    *in_degree.entry(key).or_insert(0) += 1;
                }
            }
        }
    }

    // Kahn's algorithm with engine-priority tie-breaking
    let mut queue: VecDeque<&String> = VecDeque::new();
    for key in keys {
        if in_degree.get(key).copied().unwrap_or(0) == 0 {
            queue.push_back(key);
        }
    }

    // Sort initial queue by engine priority for deterministic ordering
    let mut queue_vec: Vec<&String> = queue.into_iter().collect();
    queue_vec.sort_by_key(|k| {
        tables
            .get(*k)
            .map_or(3, |tm| engine_restore_priority(&tm.engine))
    });
    let mut queue: VecDeque<&String> = queue_vec.into_iter().collect();

    let mut result: Vec<String> = Vec::with_capacity(keys.len());

    while let Some(node) = queue.pop_front() {
        result.push(node.clone());
        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    // Check for cycles (nodes left with non-zero in-degree)
    if result.len() < keys.len() {
        let remaining: Vec<String> = keys
            .iter()
            .filter(|k| !result.contains(k))
            .cloned()
            .collect();
        warn!(
            remaining = ?remaining,
            "Dependency cycle detected among DDL-only objects, appending in engine-priority order"
        );
        let mut remaining_sorted = remaining;
        remaining_sorted.sort_by_key(|k| {
            tables
                .get(k)
                .map_or(3, |tm| engine_restore_priority(&tm.engine))
        });
        result.extend(remaining_sorted);
    }

    info!(count = result.len(), "Topological sort produced {} DDL-only objects in dependency order", result.len());
    Ok(result)
}
```

**Files:**
- `src/restore/topo.rs` -- New file
- `src/restore/mod.rs` -- Add `pub mod topo;` declaration

**Acceptance:** F003

---

### Task 4: Add create_ddl_objects() to restore/schema.rs

**Purpose:** Create the Phase 3 DDL-only object creation function with retry-loop fallback for when dependency info is unavailable.

**TDD Steps:**

1. Write unit test `test_create_ddl_objects_ordering` -- verify that DDL objects are processed in the order provided (caller is responsible for topological sort).
2. Implement `create_ddl_objects()` in src/restore/schema.rs.
3. The function iterates DDL keys (already topologically sorted by caller), creates each object, and on failure queues for retry.
4. Retry loop: up to 10 rounds -- each round retries failed objects. If a round makes no progress (no new successes), break with error.
5. Verify test passes.

**Implementation Details:**

```rust
/// Create DDL-only objects (Phase 3: dictionaries, views, MVs) in caller-provided order.
///
/// Objects are created sequentially in the provided order (which should be
/// topologically sorted by dependencies). On failure, objects are queued for
/// retry -- this handles the fallback case where dependency info is unavailable
/// and the topological sort was approximate (engine-priority only).
///
/// Max 10 retry rounds. Each round retries all previously-failed objects.
/// If a round makes zero progress (no new successes), the function returns
/// an error with the remaining failures.
pub async fn create_ddl_objects(
    ch: &ChClient,
    manifest: &BackupManifest,
    ddl_keys: &[String],
    remap: Option<&RemapConfig>,
) -> Result<()> {
    if ddl_keys.is_empty() {
        return Ok(());
    }

    let mut pending: Vec<String> = ddl_keys.to_vec();
    let max_rounds = 10;

    for round in 0..max_rounds {
        let mut failed: Vec<(String, String)> = Vec::new(); // (key, error_msg)
        let mut created_this_round = 0u32;

        for table_key in &pending {
            let table_manifest = match manifest.tables.get(table_key) {
                Some(tm) => tm,
                None => continue,
            };

            let (src_db, src_table) = table_key.split_once('.').unwrap_or(("default", table_key));
            let (dst_db, dst_table) = match remap {
                Some(rc) if rc.is_active() => rc.remap_table_key(table_key),
                _ => (src_db.to_string(), src_table.to_string()),
            };

            let exists = ch.table_exists(&dst_db, &dst_table).await.unwrap_or(false);
            if exists {
                debug!(table = %format!("{}.{}", dst_db, dst_table), "DDL object already exists");
                created_this_round += 1; // Count as progress
                continue;
            }

            if table_manifest.ddl.is_empty() {
                warn!(table = %table_key, "DDL object has no DDL in manifest");
                continue;
            }

            let ddl = match remap {
                Some(rc) if rc.is_active() && (src_db != dst_db || src_table != dst_table) => {
                    let rewritten = rewrite_create_table_ddl(
                        &table_manifest.ddl, src_db, src_table,
                        &dst_db, &dst_table, &rc.default_replica_path,
                    );
                    ensure_if_not_exists_table(&rewritten)
                }
                _ => ensure_if_not_exists_table(&table_manifest.ddl),
            };

            let dst_key = format!("{}.{}", dst_db, dst_table);
            match ch.execute_ddl(&ddl).await {
                Ok(()) => {
                    info!(table = %dst_key, engine = %table_manifest.engine, "Created DDL object");
                    created_this_round += 1;
                }
                Err(e) => {
                    if round == 0 {
                        debug!(table = %dst_key, error = %e, round = round, "DDL creation failed, will retry");
                    }
                    failed.push((table_key.clone(), e.to_string()));
                }
            }
        }

        if failed.is_empty() {
            break;
        }

        if created_this_round == 0 && round > 0 {
            // No progress this round -- give up
            let failed_keys: Vec<&str> = failed.iter().map(|(k, _)| k.as_str()).collect();
            anyhow::bail!(
                "Failed to create {} DDL-only objects after {} retry rounds: {:?}. Last errors: {}",
                failed.len(),
                round + 1,
                failed_keys,
                failed.iter().map(|(k, e)| format!("{}: {}", k, e)).collect::<Vec<_>>().join("; ")
            );
        }

        info!(
            round = round,
            created = created_this_round,
            remaining = failed.len(),
            "DDL creation retry round"
        );

        pending = failed.into_iter().map(|(k, _)| k).collect();
    }

    info!(count = ddl_keys.len(), "DDL-only objects created");
    Ok(())
}
```

**Files:**
- `src/restore/schema.rs` -- Add `create_ddl_objects()` function

**Acceptance:** F004

---

### Task 5: Restructure restore() for phased architecture

**Purpose:** Modify the `restore()` function to use `classify_restore_tables()` and `topological_sort()` to implement the phased restore architecture from design doc 5.1.

**Depends on:** Tasks 3, 4, 6 (imports classify_restore_tables, topological_sort, create_ddl_objects, create_functions)

**TDD Steps:**

1. Review existing tests in `src/restore/mod.rs` to ensure they still pass after restructuring.
2. Implement the phased flow in `restore()`:
   - After table filtering (line 107-115), call `classify_restore_tables()` to split tables into phases.
   - Phase 2: Call `create_tables()` with `phases.data_tables` (data tables only).
   - Phase 2 data: Attach parts (existing logic, but only for data tables).
   - Phase 3: Call `topological_sort()` on `phases.ddl_only_tables`, then `create_ddl_objects()`.
3. Update the import list at the top of `restore/mod.rs` to include the new functions.
4. Verify all existing tests pass.
5. Run `cargo check` to verify zero compilation errors.

**Implementation Details:**

The restructuring changes the flow inside `restore()` from:

```
create_databases -> create_tables(ALL) -> attach_parts(data only)
```

To:

```
create_databases -> classify_tables -> create_tables(data_tables) -> attach_parts(data_tables) -> topological_sort(ddl_only) -> create_ddl_objects(ddl_only) -> create_functions()
```

Key changes to src/restore/mod.rs:

1. Add imports: `use topo::{classify_restore_tables, topological_sort};` and `use schema::{create_ddl_objects, create_functions};`
2. After table_keys filtering, add: `let phases = classify_restore_tables(&manifest, &table_keys);`
3. Replace `create_tables(ch, &manifest, &table_keys, ...)` with `create_tables(ch, &manifest, &phases.data_tables, ...)`
4. The data attach loop iterates `phases.data_tables` instead of `table_keys`
5. After data attach completion, add Phase 3:
   ```rust
   // Phase 3: DDL-only objects (topologically sorted)
   if !data_only && !phases.ddl_only_tables.is_empty() {
       let sorted_ddl = topological_sort(&manifest.tables, &phases.ddl_only_tables)?;
       create_ddl_objects(ch, &manifest, &sorted_ddl, remap_ref).await?;
   }
   ```
6. After Phase 3, add Phase 4:
   ```rust
   // Phase 4: Functions
   if !data_only && !manifest.functions.is_empty() {
       create_functions(ch, &manifest).await?;
   }
   ```

**Files:**
- `src/restore/mod.rs` -- Restructure restore() flow, add imports

**Acceptance:** F005

---

### Task 6: Add create_functions() to restore/schema.rs

**Purpose:** Create functions from the manifest's `functions` field. This is Phase 4 of the restore architecture per design doc 5.6.

**TDD Steps:**

1. Write unit test `test_create_functions_skips_empty` -- verify empty function list returns immediately.
2. Implement `create_functions()` in src/restore/schema.rs.
3. For each function DDL in `manifest.functions`, execute via `ch.execute_ddl()`.
4. Use IF NOT EXISTS safety (functions DDL stored as `CREATE FUNCTION ...`).
5. Verify test passes.

**Implementation Details:**

```rust
/// Create functions from the manifest (Phase 4: functions, named collections, RBAC).
///
/// Each entry in `manifest.functions` is a complete `CREATE FUNCTION` DDL statement.
/// Functions are created sequentially since they typically have no inter-dependencies.
pub async fn create_functions(ch: &ChClient, manifest: &BackupManifest) -> Result<()> {
    if manifest.functions.is_empty() {
        debug!("No functions to create");
        return Ok(());
    }

    let mut created = 0u32;
    for func_ddl in &manifest.functions {
        match ch.execute_ddl(func_ddl).await {
            Ok(()) => {
                info!(ddl = %func_ddl, "Created function");
                created += 1;
            }
            Err(e) => {
                // Log warning but continue -- function may already exist
                warn!(ddl = %func_ddl, error = %e, "Failed to create function, continuing");
            }
        }
    }

    info!(created = created, total = manifest.functions.len(), "Function creation phase complete");
    Ok(())
}
```

**Files:**
- `src/restore/schema.rs` -- Add `create_functions()` function

**Acceptance:** F006

---

### Task 7: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/backup, src/restore, src/clickhouse

**TDD Steps:**

1. Read `context/affected-modules.json` for module list.
2. For each module, regenerate directory tree.
3. Detect and add new patterns:
   - src/clickhouse: Document `DependencyRow` (private), `query_table_dependencies()` method, graceful fallback for CH < 23.3.
   - src/backup: Document dependency population in `create()`, `Arc<HashMap>` sharing pattern for spawned tasks.
   - src/restore: Document `topo.rs` module, phased restore architecture, `create_ddl_objects()` retry loop, `create_functions()`, `RestorePhases` struct.
4. Validate all CLAUDE.md files have required sections (Parent Context, Directory Structure, Key Patterns, Parent Rules).

**Files:**
- `src/backup/CLAUDE.md`
- `src/restore/CLAUDE.md`
- `src/clickhouse/CLAUDE.md`

**Acceptance:** FDOC

---

## Notes

### Phase 4.5: Interface Skeleton Simulation

Skipped. Rationale: All new code uses only existing crate types (`HashMap`, `Vec`, `String`, `Result`) and existing project types verified in `context/knowledge_graph.json`. No new generic types, no new trait bounds. The new `DependencyRow` struct uses the same `#[derive(clickhouse::Row, serde::Deserialize)]` pattern as existing row types. All imports are verified in the knowledge graph.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All APIs verified: `query_table_dependencies` (new), `execute_ddl` (exists client.rs), `table_exists` (exists client.rs), `list_tables` (exists client.rs:250) |
| RC-008 | PASS | Task sequencing: Task 1 before Task 2; Tasks 3, 4, 6 before Task 5; Task 5 imports from Tasks 3+4+6 |
| RC-015 | PASS | Data flow: Task 2 outputs `Vec<String>` deps -> serialized in manifest -> Task 3/5 reads from manifest.tables[key].dependencies |
| RC-016 | PASS | RestorePhases struct fields (data_tables, postponed_tables, ddl_only_tables) all consumed in Task 5 |
| RC-017 | PASS | No new self.X fields -- all new state is local variables in function bodies |
| RC-018 | PASS | Every task has named test functions with specific assertions |
| RC-019 | PASS | query_table_dependencies follows list_tables pattern; create_ddl_objects follows create_tables pattern |
| RC-021 | PASS | File locations verified: ChClient in client.rs:14, BackupManifest in manifest.rs:19, create_tables in schema.rs:113, restore in mod.rs:62 |

### Cross-Task Type Consistency

- `query_table_dependencies()` returns `HashMap<String, Vec<String>>` -- Task 2 consumes this exact type
- `classify_restore_tables()` returns `RestorePhases` with `Vec<String>` fields -- Task 5 passes these to `create_tables()` and `topological_sort()` which both take `&[String]`
- `topological_sort()` returns `Result<Vec<String>>` -- Task 5 passes this to `create_ddl_objects()` which takes `&[String]`
- `create_ddl_objects()` and `create_functions()` both use `ch.execute_ddl()` which exists at src/clickhouse/client.rs

### Redundancy Analysis (from context/redundancy-analysis.md)

All COEXIST decisions validated:
- `query_table_dependencies()` coexists with `list_tables()` -- different column sets, no caller breakage
- `create_ddl_objects()` coexists with `create_tables()` -- different retry semantics, called in different phases
- No REPLACE decisions require removal tasks
