# Plan: Phase 4c -- Streaming Engine Postponement

## Goal

Implement Phase 2b of the restore architecture: detect streaming engines (Kafka, NATS, RabbitMQ, S3Queue) and refreshable MVs during table classification, and postpone their CREATE until AFTER all data tables have their parts attached. This prevents streaming engines from consuming data prematurely during restore (#1235).

## Architecture Overview

Design doc section 5.1 defines a phased restore architecture. Phase 4b (completed) implemented Phases 1-4 with a placeholder for Phase 2b. This plan fills in Phase 2b:

```
Phase 1:  CREATE databases           (existing)
Phase 2:  CREATE + ATTACH data tables (existing)
Phase 2b: CREATE postponed tables     (THIS PLAN -- streaming engines + refreshable MVs)
Phase 3:  CREATE DDL-only objects     (existing)
Phase 4:  CREATE functions            (existing)
```

The change involves three modules:
1. **topo.rs** -- Add `is_streaming_engine()` and `is_refreshable_mv()` detection functions; modify `classify_restore_tables()` to populate `postponed_tables`
2. **mod.rs** -- Add Phase 2b execution block between data attachment (line 432) and Phase 3 DDL (line 434); handle schema-only and data-only modes; update logging
3. **CLAUDE.md** -- Update module documentation for Phase 2b

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **`RestorePhases.postponed_tables`**: Already defined in `src/restore/topo.rs:55`, currently hardcoded to `Vec::new()` at line 89. This plan populates it.
- **`classify_restore_tables()`**: Owned by `topo.rs`, called from `mod.rs:144`. We modify the classification logic but not the signature.
- **`create_tables()`**: Owned by `schema.rs:113`. Reused as-is for postponed tables (generic enough to accept any `&[String]` key slice).
- **`BackupManifest.tables`**: Owned by `manifest.rs`. Read-only access via `HashMap<String, TableManifest>`. Fields used: `engine` (String), `ddl` (String), `metadata_only` (bool).

### What This Plan CANNOT Do
- Cannot modify the backup pipeline -- streaming engines may still be backed up as non-metadata-only data tables with empty parts
- Cannot test with real Kafka/NATS brokers -- streaming engine tables require broker configuration that does not exist in the test environment. Tests use manifest-level classification checks.
- Cannot guarantee ClickHouse >= 24.1 for REFRESH MVs -- detection is DDL-based, so older CH versions simply have no refreshable MVs to detect

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Streaming engine names differ from expected strings | GREEN | ClickHouse engine names are stable across versions. The four engines (Kafka, NATS, RabbitMQ, S3Queue) are well-known. |
| REFRESH keyword in table/column names causes false positive | GREEN | Detection checks `engine == "MaterializedView"` AND DDL contains REFRESH as a SQL keyword (preceded by whitespace/newline). Column names would appear after SELECT, not before ENGINE. |
| Postponed tables with empty `ddl` field | GREEN | `create_tables()` already skips tables with empty DDL and logs a warning. |
| data_only mode creates postponed tables | GREEN | `create_tables()` already has `data_only` early return. We pass `data_only` to the Phase 2b call. |
| Remap support for postponed tables | GREEN | `create_tables()` already handles remap. We pass `remap_ref` to the Phase 2b call. |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Classified .* tables: .* data, .* postponed, .* DDL-only` | yes | Updated classification log showing all three categories |
| `Phase 2b: .* postponed tables` | yes | Phase 2b execution log (only when postponed_tables is non-empty) |
| `ERROR:.*` | no (forbidden) | Should NOT appear during normal restore with streaming engines |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Streaming engines in backup classification | Different module (backup/mod.rs), not needed for restore correctness | Could add to `is_metadata_only_engine()` in a future plan |
| T12 integration test with real Kafka | Requires Kafka broker in test infrastructure | Phase 4c tests use unit tests on classification logic |
| Streaming engine ordering within Phase 2b | Only relevant with many streaming tables; current plan uses insertion order | Could add priority sort if needed |

## Dependency Groups

```
Group A (Sequential):
  - Task 1: Add is_streaming_engine() and is_refreshable_mv() to topo.rs
  - Task 2: Modify classify_restore_tables() to populate postponed_tables (depends on Task 1)
  - Task 3: Add Phase 2b execution block in mod.rs (depends on Task 2)

Group B (After Group A):
  - Task 4: Update CLAUDE.md for src/restore (depends on Tasks 1-3)
```

## Tasks

### Task 1: Add streaming engine and refreshable MV detection functions

**TDD Steps:**

1. **Write failing test: `test_is_streaming_engine`**
   - In `src/restore/topo.rs` test module
   - Assert `is_streaming_engine("Kafka") == true`
   - Assert `is_streaming_engine("NATS") == true`
   - Assert `is_streaming_engine("RabbitMQ") == true`
   - Assert `is_streaming_engine("S3Queue") == true`
   - Assert `is_streaming_engine("MergeTree") == false`
   - Assert `is_streaming_engine("MaterializedView") == false`
   - Assert `is_streaming_engine("View") == false`

2. **Write failing test: `test_is_refreshable_mv`**
   - In `src/restore/topo.rs` test module
   - Use existing `make_table_manifest()` helper (defined at line 216)
   - Create a TableManifest with engine="MaterializedView" and DDL containing "REFRESH EVERY 1 HOUR"
   - Assert `is_refreshable_mv(&tm) == true`
   - Create a regular MV without REFRESH clause
   - Assert `is_refreshable_mv(&tm) == false`
   - Create a non-MV table with REFRESH in DDL (edge case)
   - Assert `is_refreshable_mv(&tm) == false`
   - Test case-insensitive: DDL with "refresh every" (lowercase)
   - Assert `is_refreshable_mv(&tm) == true`

3. **Implement `is_streaming_engine(engine: &str) -> bool`**
   - In `src/restore/topo.rs`, add as `pub` function (needed by both topo.rs and potentially other modules)
   - Use `matches!()` macro pattern matching `is_metadata_only_engine()` in backup/mod.rs
   - Match: "Kafka" | "NATS" | "RabbitMQ" | "S3Queue"

4. **Implement `is_refreshable_mv(tm: &TableManifest) -> bool`**
   - In `src/restore/topo.rs`, add as `pub` function
   - Check: `tm.engine == "MaterializedView"` AND `tm.ddl` contains the word "REFRESH" (case-insensitive)
   - Use `.to_uppercase().contains(" REFRESH ")` or similar for case-insensitive word boundary check
   - Also check start-of-DDL edge case: handle `\nREFRESH` (newline before REFRESH)

5. **Verify tests pass**

6. **Refactor: ensure no warnings (unused import, etc.)**

**Files:** `src/restore/topo.rs`
**Acceptance:** F001

**Implementation Notes:**
- `make_table_manifest()` helper (topo.rs:216) takes `(engine: &str, metadata_only: bool, deps: Vec<String>)` but creates DDL as `format!("CREATE TABLE test (id UInt64) ENGINE = {}", engine)`. For refreshable MV tests, we need to manually set the `ddl` field on the returned struct.
- The `is_refreshable_mv` function takes `&TableManifest` (not just engine string) because it needs both `engine` and `ddl` fields.
- For the REFRESH detection: ClickHouse DDL places the REFRESH clause between the view name and the ENGINE clause, e.g., `CREATE MATERIALIZED VIEW ... REFRESH EVERY 1 HOUR ENGINE = MergeTree()`. The word REFRESH will always be preceded by whitespace or newline.

---

### Task 2: Modify classify_restore_tables() to populate postponed_tables

**TDD Steps:**

1. **Write failing test: `test_classify_streaming_engines_postponed`**
   - In `src/restore/topo.rs` test module
   - Create manifest with:
     - `default.trades` (MergeTree, metadata_only=false) -> should be data_tables
     - `default.kafka_source` (Kafka, metadata_only=false) -> should be postponed_tables
     - `default.my_view` (View, metadata_only=true) -> should be ddl_only_tables
   - Call `classify_restore_tables(&manifest, &all_keys)`
   - Assert `phases.data_tables` contains "default.trades" (len 1)
   - Assert `phases.postponed_tables` contains "default.kafka_source" (len 1)
   - Assert `phases.ddl_only_tables` contains "default.my_view" (len 1)

2. **Write failing test: `test_classify_refreshable_mv_postponed`**
   - Create manifest with:
     - `default.trades` (MergeTree, metadata_only=false)
     - `default.refresh_mv` (MaterializedView, metadata_only=true, DDL with "REFRESH EVERY 1 HOUR")
     - `default.regular_mv` (MaterializedView, metadata_only=true, DDL without REFRESH)
   - Call `classify_restore_tables(&manifest, &all_keys)`
   - Assert `phases.data_tables` contains "default.trades" (len 1)
   - Assert `phases.postponed_tables` contains "default.refresh_mv" (len 1)
   - Assert `phases.ddl_only_tables` contains "default.regular_mv" (len 1)

3. **Write failing test: `test_classify_all_streaming_engines`**
   - Test all four streaming engines: Kafka, NATS, RabbitMQ, S3Queue
   - All should land in `postponed_tables`

4. **Modify `classify_restore_tables()` in topo.rs**
   - Add `let mut postponed_tables: Vec<String> = Vec::new();`
   - Change the classification loop to use the new decision tree:
     ```
     1. If is_streaming_engine(&tm.engine) -> postponed_tables (regardless of metadata_only)
     2. If is_refreshable_mv(tm) -> postponed_tables (engine is MaterializedView + REFRESH in DDL)
     3. If tm.metadata_only -> ddl_only_tables
     4. Else -> data_tables
     ```
   - Update the `info!()` log to include postponed count: `"Classified {} tables: {} data, {} postponed, {} DDL-only"`
   - Replace `postponed_tables: Vec::new()` with `postponed_tables` in the returned struct

5. **Verify existing test `test_classify_restore_tables_basic` still passes**
   - The existing test at line 276 asserts `phases.postponed_tables.is_empty()` -- this assertion remains valid because the test manifest has no streaming engines or refreshable MVs.

6. **Verify all tests pass**

**Files:** `src/restore/topo.rs`
**Acceptance:** F002

**Implementation Notes:**
- The `make_table_manifest()` helper creates DDL as `format!("CREATE TABLE test (...) ENGINE = {}", engine)`. For refreshable MV tests, modify the `ddl` field after creation:
  ```rust
  let mut tm = make_table_manifest("MaterializedView", true, vec![]);
  tm.ddl = "CREATE MATERIALIZED VIEW default.refresh_mv REFRESH EVERY 1 HOUR ENGINE = MergeTree() ORDER BY symbol AS SELECT symbol, count() FROM default.trades GROUP BY symbol".to_string();
  ```
- Streaming engines check takes priority over metadata_only check. This is important because Kafka/NATS/RabbitMQ/S3Queue are NOT metadata_only (metadata_only=false), so the current binary split would put them in `data_tables`. The new logic must check streaming engine FIRST.
- Refreshable MVs are metadata_only=true (engine is MaterializedView which is in `is_metadata_only_engine()`), so without the new check they would go to `ddl_only_tables`. The REFRESH detection diverts them to `postponed_tables` instead.

---

### Task 3: Add Phase 2b execution block in restore mod.rs

**TDD Steps:**

1. **Implement Phase 2b block in `restore()` function**
   - Location: Between line 432 (tally totals) and line 434 (Phase 3 DDL-only objects)
   - Insert Phase 2b block (follows existing Phase 3 pattern with `!data_only` guard):
     ```rust
     // Phase 2b: Postponed tables (streaming engines, refreshable MVs)
     // Created AFTER all data is attached, BEFORE DDL-only objects (#1235)
     if !data_only && !phases.postponed_tables.is_empty() {
         info!(
             count = phases.postponed_tables.len(),
             "Phase 2b: {} postponed tables",
             phases.postponed_tables.len()
         );
         create_tables(ch, &manifest, &phases.postponed_tables, data_only, remap_ref).await?;
     }
     ```

2. **Handle schema-only mode**
   - In the schema-only early return block (line 160-175), add Phase 2b creation:
     ```rust
     // Schema-only mode: also create postponed tables
     if !data_only && !phases.postponed_tables.is_empty() {
         info!(
             count = phases.postponed_tables.len(),
             "Phase 2b: {} postponed tables (schema-only)",
             phases.postponed_tables.len()
         );
         create_tables(ch, &manifest, &phases.postponed_tables, data_only, remap_ref).await?;
     }
     ```
   - In schema-only mode, postponed tables are created AFTER Phase 3 DDL-only objects (since there is no data to attach, ordering matters less, but Phase 3 objects may be targets for streaming engines)
   - The ordering in schema-only should be: Phase 2 data tables -> Phase 3 DDL-only -> Phase 2b postponed (since DDL-only objects like regular MVs may be targets that streaming engines write to)

3. **Verify data-only mode is handled**
   - `create_tables()` already has `if data_only { return Ok(()) }` at line 120-123
   - When `data_only=true`, the Phase 2b call to `create_tables()` will correctly skip DDL creation
   - No additional code needed -- the existing guard handles it

4. **Update module doc comment**
   - Update the module-level doc comment in mod.rs (line 1-9) to include Phase 2b:
     ```rust
     //! 3b. Phase 2b: CREATE postponed tables (streaming engines, refreshable MVs)
     ```

5. **Verify compilation: `cargo check`**

6. **Verify existing tests pass: `cargo test -p chbackup`**

**Files:** `src/restore/mod.rs`
**Acceptance:** F003

**Implementation Notes:**
- The `create_tables()` function signature is: `pub async fn create_tables(ch: &ChClient, manifest: &BackupManifest, table_keys: &[String], data_only: bool, remap: Option<&RemapConfig>) -> Result<()>` (from knowledge_graph.json).
- In schema-only mode, the order should be: data table DDL -> DDL-only objects -> postponed tables (after DDL-only, since DDL-only objects like regular MVs may be targets that streaming engines write to).
- In full restore mode, the order is: data table DDL -> data attach -> Phase 2b postponed -> Phase 3 DDL-only -> Phase 4 functions.
- The `!data_only` guard in the Phase 2b block follows the existing Phase 3 pattern at line 435 (`if !data_only && !phases.ddl_only_tables.is_empty()`). While `create_tables()` also checks `data_only` internally, the outer guard avoids the info! log and is consistent with the surrounding code.

---

### Task 4: Update CLAUDE.md for src/restore (MANDATORY)

**TDD Steps:**

1. **Read `src/restore/CLAUDE.md`** for current content

2. **Regenerate directory tree:**
   ```bash
   tree -L 2 src/restore --noreport 2>/dev/null || ls -la src/restore
   ```

3. **Add Phase 2b documentation to Key Patterns section:**
   - Add "Streaming Engine Postponement (Phase 4c)" subsection
   - Document `is_streaming_engine()`, `is_refreshable_mv()`, and Phase 2b execution
   - Update the "Phased Restore Architecture" subsection to include Phase 2b

4. **Update Public API section:**
   - Add `is_streaming_engine(engine) -> bool` and `is_refreshable_mv(tm) -> bool`

5. **Validate required sections exist:**
   - Parent Context
   - Directory Structure
   - Key Patterns
   - Parent Rules

**Files:** `src/restore/CLAUDE.md`
**Acceptance:** FDOC

**Notes:**
- This task runs AFTER all code tasks complete
- Preserve existing patterns, only ADD new ones
- Use Edit tool to update sections, preserving rest of file

## Notes

### Phase 4.5 -- Interface Skeleton Simulation

**Skip reason:** All changes modify existing functions or add simple `pub fn` utility functions that take `&str` or `&TableManifest` arguments. No new imports from external crates are needed. No new struct definitions. The only types used (`TableManifest`, `BackupManifest`, `RestorePhases`) are already verified in `context/knowledge_graph.json`. A stub check would be redundant for this plan.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All APIs verified: `classify_restore_tables`, `create_tables`, `is_streaming_engine` (new), `is_refreshable_mv` (new), `make_table_manifest` (test helper) |
| RC-008 | PASS | Task 1 defines detection functions, Task 2 uses them in classification, Task 3 uses Task 2's output in mod.rs |
| RC-015 | PASS | `classify_restore_tables` returns `RestorePhases` with `postponed_tables: Vec<String>` -- `create_tables` accepts `&[String]`, types match |
| RC-018 | PASS | Every task has named test functions with specific assertions |
| RC-019 | PASS | New functions follow `matches!()` pattern from `is_metadata_only_engine()` and `engine_restore_priority()` |
| RC-021 | PASS | File locations verified: `RestorePhases` at topo.rs:51, `classify_restore_tables` at topo.rs:61, `create_tables` at schema.rs:113, restore flow at mod.rs:62 |

### Cross-Task Type Consistency

- Task 1 defines `is_streaming_engine(engine: &str) -> bool` -- Task 2 calls `is_streaming_engine(&tm.engine)` where `tm.engine` is `String` -- `&String` coerces to `&str`, correct.
- Task 1 defines `is_refreshable_mv(tm: &TableManifest) -> bool` -- Task 2 calls `is_refreshable_mv(tm)` where `tm` is `&TableManifest` from `manifest.tables.get(key)` -- correct.
- Task 2 populates `phases.postponed_tables: Vec<String>` -- Task 3 passes `&phases.postponed_tables` to `create_tables(ch, &manifest, &phases.postponed_tables, data_only, remap_ref)` where parameter type is `&[String]` -- `&Vec<String>` coerces to `&[String]`, correct.

### Redundancy Consistency

Per `context/redundancy-analysis.md`:
- `is_streaming_engine()` COEXISTS with `is_engine_excluded()` -- different purposes (hardcoded safety vs user config). No removal task needed.
- `is_refreshable_mv()` -- no existing equivalent. New function, no redundancy.
