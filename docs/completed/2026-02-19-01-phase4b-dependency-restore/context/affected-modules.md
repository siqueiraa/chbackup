# Affected Modules Analysis

## Summary

- **Modules to update:** 3
- **Modules to create:** 0
- **New files:** 1 (src/restore/topo.rs)
- **Git base:** ca7297a1

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action | Changes |
|--------|------------------|----------|--------|---------|
| src/backup | EXISTS | new_patterns | UPDATE | Populate `dependencies` field in `create()` using `ch.query_table_dependencies()` |
| src/restore | EXISTS | new_patterns, tree_change | UPDATE | Add `topo.rs` module; restructure `restore()` for phased architecture; add `create_ddl_objects()` and `create_functions()` in `schema.rs` |
| src/clickhouse | EXISTS | new_patterns | UPDATE | Add `DependencyRow` struct (private) and `query_table_dependencies()` method |

## Files to Modify

| File | Change Type | Description |
|------|-------------|-------------|
| src/clickhouse/client.rs | ADD struct + method | `DependencyRow` (private), `query_table_dependencies() -> HashMap<String, Vec<String>>` |
| src/backup/mod.rs | MODIFY | Call `query_table_dependencies()` after `list_tables()`, populate `TableManifest.dependencies` for metadata-only AND data tables |
| src/restore/topo.rs | NEW FILE | `RestorePhases` struct, `classify_restore_tables()`, `topological_sort()`, `engine_restore_priority()` |
| src/restore/mod.rs | MODIFY | Add `pub mod topo;` declaration, restructure `restore()` to use classify -> Phase 2 (data) -> Phase 3 (DDL-only) -> Phase 4 (functions) |
| src/restore/schema.rs | MODIFY | Add `create_ddl_objects()` (topo-sorted with retry fallback), add `create_functions()` |

## Files NOT Modified

| File | Reason |
|------|--------|
| src/main.rs | `restore()` function signature unchanged; all new behavior is internal |
| src/lib.rs | Module declarations already complete |
| src/manifest.rs | `TableManifest.dependencies` field already exists (just needs population) |
| src/clickhouse/mod.rs | `query_table_dependencies()` doesn't return a new public type (returns HashMap) |
| src/cli.rs | No new CLI flags for this feature |
| src/config.rs | No new config params needed |

## Architecture Assumptions (VALIDATED)

### Component Ownership
- `BackupManifest`: Created by `backup::create()`, serialized to JSON by `save_to_file()`, loaded by `restore::restore()` via `load_from_file()`
- `TableManifest.dependencies`: Field exists (manifest.rs:116), always `Vec::new()` (backup/mod.rs:249) -- must be populated
- `TableRow`: Defined in client.rs:25, returned by `list_tables()`, consumed by backup and restore
- `ChClient`: Created in main.rs, cloned into backup/restore functions

### What This Plan CANNOT Do
- Cannot change `restore()` function signature (would break main.rs, server routes, watch module callers)
- Cannot change `TableRow` struct fields (would break all callers of `list_tables()`)
- Cannot rely on CH dependency columns for CH < 23.3 (must have fallback)
- Cannot test dependency ordering without real ClickHouse (integration test only)

## CLAUDE.md Tasks to Generate

1. **Update:** src/backup/CLAUDE.md -- document dependency population pattern
2. **Update:** src/restore/CLAUDE.md -- document topo.rs, phased restore, create_ddl_objects
3. **Update:** src/clickhouse/CLAUDE.md -- document DependencyRow, query_table_dependencies
