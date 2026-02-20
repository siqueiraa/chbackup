# Pattern Discovery

No global `docs/patterns/` directory exists. Patterns discovered locally from codebase analysis.

## Pattern 1: Engine Classification in Backup (is_metadata_only_engine)

**Location:** `src/backup/mod.rs:589-604`

**Pattern:** A `matches!()` macro on engine string to classify engine types. Returns `bool`.

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

**Relevance:** Similar pattern needed for `is_streaming_engine()` detection in restore. Follow same `matches!()` macro style. Function should be `pub` since topo.rs and mod.rs both need it.

## Pattern 2: Engine Priority Classification in Restore (engine_restore_priority)

**Location:** `src/restore/topo.rs:40-47`

**Pattern:** A `match` expression returning `u8` priority for DDL-only object ordering.

```rust
pub fn engine_restore_priority(engine: &str) -> u8 {
    match engine {
        "Dictionary" => 0,
        "View" | "MaterializedView" | "LiveView" | "WindowView" => 1,
        "Distributed" | "Merge" => 2,
        _ => 3,
    }
}
```

**Relevance:** New streaming engine detection function should follow the same simple `match`/`matches!` pattern.

## Pattern 3: Table Classification Loop (classify_restore_tables)

**Location:** `src/restore/topo.rs:61-92`

**Pattern:** Iterates table keys, looks up `TableManifest` from manifest, classifies into buckets based on engine/metadata_only, returns `RestorePhases` struct.

```rust
pub fn classify_restore_tables(manifest: &BackupManifest, table_keys: &[String]) -> RestorePhases {
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
    // sort, log, return RestorePhases { data_tables, postponed_tables: Vec::new(), ddl_only_tables }
}
```

**Relevance:** MUST extend this loop to add a third classification path for streaming engines and refreshable MVs. The `postponed_tables` field already exists in `RestorePhases` (currently hardcoded to `Vec::new()`).

**Classification decision tree (new):**
1. If `is_streaming_engine(&tm.engine)` -> `postponed_tables` (regardless of `metadata_only`)
2. If `is_refreshable_mv(tm)` -> `postponed_tables` (already metadata_only=true)
3. If `tm.metadata_only` -> `ddl_only_tables`
4. Else -> `data_tables`

## Pattern 4: Phase-Based Restore Orchestration (restore mod.rs)

**Location:** `src/restore/mod.rs:144-462`

**Pattern:** The restore function calls phases sequentially:
1. `create_databases(ch, manifest, remap)` -- Phase 1
2. `create_tables(ch, manifest, &phases.data_tables, data_only, remap)` -- Phase 2
3. Data attachment loop on `phases.data_tables` -- Phase 2 data
4. [GAP - Phase 2b goes here, after line 432, before line 434]
5. `create_ddl_objects(ch, manifest, &sorted_ddl, remap)` -- Phase 3
6. `create_functions(ch, manifest)` -- Phase 4

**Insertion point for Phase 2b:** Between the data attachment tally (line 432) and Phase 3 DDL-only objects (line 434). The postponed tables are created AFTER all data is attached but BEFORE DDL-only objects.

**Phase 2b uses `create_tables()`**: The existing `create_tables()` function already handles creating tables from DDL with IF NOT EXISTS safety and remap support. Postponed tables are just regular tables whose CREATE is delayed -- no special DDL handling needed.

## Pattern 5: Engine Exclusion During Backup (skip_table_engines)

**Location:** `src/backup/mod.rs:141` + `src/table_filter.rs:78-80`

**Pattern:** Config-driven engine exclusion via exact string match. Used only during backup.

**Relevance:** Reference only. The `skip_table_engines` config is a user choice to exclude engines from backup. Streaming engine postponement is a hardcoded safety measure during restore. Different purpose, no reuse.

## Pattern 6: DDL-Based Detection (REFRESH clause)

**Location:** Design doc section 5.1

**Pattern:** Refreshable MVs are identified by the `REFRESH` clause in their `CREATE MATERIALIZED VIEW` DDL, not by engine name (engine is still "MaterializedView"). Detection requires scanning the DDL string.

**Relevance:** Need a function that checks `table_manifest.ddl` for `REFRESH` keyword. Must be case-insensitive since SQL is case-insensitive. The check should be on the word "REFRESH" preceded by whitespace or start-of-line to avoid false positives on table/column names containing "refresh".
