# Symbol and Reference Analysis

## Phase 4c Target Symbols

### 1. `RestorePhases` (struct) -- `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/topo.rs:49-58`

```rust
#[derive(Debug, Clone)]
pub struct RestorePhases {
    pub data_tables: Vec<String>,
    pub postponed_tables: Vec<String>,  // Phase 4c target
    pub ddl_only_tables: Vec<String>,
}
```

**References to `RestorePhases`:** (LSP findReferences returned too broad; scoped analysis below)

- **Definition:** `src/restore/topo.rs:49`
- **Construction:** `src/restore/topo.rs:87-91` (in `classify_restore_tables`)
- **Consumer:** `src/restore/mod.rs:144` (in `restore()`)
- **Import:** `src/restore/mod.rs:42` (`use topo::{classify_restore_tables, topological_sort}`)

### 2. `postponed_tables` field -- `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/topo.rs:55`

**References (2 total, all in topo.rs):**
- `src/restore/topo.rs:55` -- field definition
- `src/restore/topo.rs:89` -- hardcoded `Vec::new()` assignment (the line to change)
- `src/restore/topo.rs:302` -- test assertion `assert!(phases.postponed_tables.is_empty())`

**NOT referenced in `restore/mod.rs`**: The `postponed_tables` field is never read in the restore pipeline. Phase 4c must add the Phase 2b execution block between data attachment (line ~425) and Phase 3 DDL (line ~434).

### 3. `classify_restore_tables()` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/topo.rs:61`

**Signature:**
```rust
pub fn classify_restore_tables(manifest: &BackupManifest, table_keys: &[String]) -> RestorePhases
```

**References (5 total):**
- `src/restore/topo.rs:61` -- definition
- `src/restore/topo.rs:306, 344` -- tests
- `src/restore/mod.rs:42` -- import
- `src/restore/mod.rs:144` -- call site in `restore()`

**Current classification logic (line 66-71):**
```rust
for key in table_keys {
    if let Some(tm) = manifest.tables.get(key) {
        if tm.metadata_only {
            ddl_only_tables.push(key.clone());
        } else {
            data_tables.push(key.clone());
        }
    }
}
```
This is a binary split: `metadata_only=true` -> DDL-only, `metadata_only=false` -> data. Phase 4c adds a third branch: streaming/refreshable engines go to `postponed_tables`.

### 4. `engine_restore_priority()` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/topo.rs:40`

**References (4 total, all in topo.rs):** Used for tie-breaking in topological sort.

### 5. `TableManifest.engine` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/manifest.rs:96`

**Type:** `String`
**Usage in classification:** The `engine` field is a plain string like "MergeTree", "Kafka", "MaterializedView", etc. It is NOT an enum -- classification must be string matching.

### 6. `TableManifest.ddl` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/manifest.rs:88`

**Type:** `String`
**Usage:** Stores the full `CREATE TABLE` DDL statement. For refreshable MV detection, we need to check if this DDL contains the `REFRESH` keyword.

### 7. `is_metadata_only_engine()` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/backup/mod.rs:589-604`

```rust
fn is_metadata_only_engine(engine: &str) -> bool {
    matches!(
        engine,
        "View" | "MaterializedView" | "LiveView" | "WindowView"
            | "Dictionary" | "Null" | "Set" | "Join" | "Buffer"
            | "Distributed" | "Merge"
    )
}
```

**Key observation:** Kafka, NATS, RabbitMQ, S3Queue are NOT in this list. They are NOT metadata_only. This means during backup, they would be classified as data tables (metadata_only=false). During restore, they currently go into `data_tables` phase, which is the bug Phase 4c fixes.

### 8. `is_engine_excluded()` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/table_filter.rs:78-80`

```rust
pub fn is_engine_excluded(engine: &str, skip_engines: &[String]) -> bool {
    skip_engines.iter().any(|e| e == engine)
}
```

Already exists for backup-time filtering via `skip_table_engines` config. Not directly relevant to restore classification but shows the pattern for engine string matching.

### 9. `create_tables()` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/schema.rs:113-186`

Called for Phase 2 data tables. For Phase 2b, postponed tables need their own CREATE call that runs AFTER all data is attached.

### 10. `create_ddl_objects()` -- `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/schema.rs:198-313`

Called for Phase 3 DDL-only objects. Has retry-loop pattern that may be useful reference for postponed CREATE.

## Insertion Point Analysis

The restore flow in `src/restore/mod.rs` has this structure:

```
Line 147-149: Phase 1: CREATE databases
Line 152-157: Phase 2: CREATE data tables + data attach
Line 160-175: Schema-only early return (creates DDL objects + functions)
Line 178-248: Resume state loading
Line 251-314: Setup (ownership, S3 client, disk paths, etc.)
Line 316-386: Build restore_items for data tables
Line 388-425: Parallel table restore (semaphore + try_join_all)
Line 427-432: Tally totals
-- Phase 2b INSERT POINT HERE (between data attach and Phase 3 DDL)
Line 434-443: Phase 3: DDL-only objects (topologically sorted)
Line 445-448: Phase 4: Functions
Line 450-462: Summary + cleanup
```

**Phase 2b insertion point:** Between line 432 (tally totals) and line 434 (Phase 3 DDL-only). This is where postponed tables should be CREATEd.

## Data Flow Analysis

The data flow for streaming engine classification:

```
manifest.tables[key].engine -> classify_restore_tables() -> RestorePhases.postponed_tables
manifest.tables[key].ddl (for REFRESH detection) -> classify_restore_tables() -> RestorePhases.postponed_tables
```

For postponed table creation:

```
RestorePhases.postponed_tables -> create_tables() (reusing existing function) -> ch.execute_ddl()
```

The `create_tables()` function already handles IF NOT EXISTS and remap. Postponed tables just need to be passed to it at the right time.

## Streaming Engines to Detect (from design doc section 5.1)

Per design doc section 5.1 (Phase 2b):
- **Kafka** -- starts consuming immediately on CREATE
- **NATS** -- same behavior
- **RabbitMQ** -- same behavior
- **S3Queue** -- same behavior
- **Refreshable MVs** -- MVs with `REFRESH` clause in DDL (CH >= 24.1)

These engines start consuming data immediately upon creation, so they must be created AFTER all target tables have their data restored.

## Important: metadata_only vs streaming

Streaming engines like Kafka are **NOT** metadata_only. They have a real engine but typically no data parts in the backup. The `metadata_only` flag is set during backup based on `is_metadata_only_engine()`, and Kafka etc. are NOT in that list.

However, looking at the backup flow, Kafka tables would likely have no frozen data parts (Kafka tables don't have local data in the same way MergeTree does). They might still be classified as data tables with empty parts, which means:
- They go into `data_tables` in classify_restore_tables
- In the data attach loop, they get skipped (empty parts -> `continue`)
- But their CREATE TABLE already happened at Phase 2 time

Phase 4c needs to:
1. Move streaming engine tables from `data_tables` to `postponed_tables`
2. NOT create them during Phase 2
3. Create them during Phase 2b (after data attach completes)
