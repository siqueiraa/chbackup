# Redundancy Analysis

## New Public Components Proposed

| Proposed | Existing Match (fully qualified) | Decision | Task/Acceptance | Justification |
|----------|----------------------------------|----------|-----------------|---------------|
| `ChClient::query_table_dependencies()` | `ChClient::list_tables()` (client.rs:250) | COEXIST | Cleanup: merge into list_tables in future CH client refactor | Separate query for deps columns (Array(String) types) avoids breaking all list_tables() callers. Only called from backup::create(). |
| `DependencyRow` struct (local to client.rs) | `TableRow` (client.rs:25) | COEXIST | Cleanup: could be merged into TableRow with optional fields in future refactor | Different field set (only db, name, deps arrays). Minimal struct avoids bloating TableRow for callers that don't need deps. |
| `topo::engine_restore_priority(engine) -> u8` | None found | N/A | - | New function, no existing equivalent |
| `topo::topological_sort(tables) -> Result<Vec<String>>` | None found | N/A | - | New function, no existing equivalent |
| `topo::classify_restore_tables(manifest) -> RestorePhases` | None found | N/A | - | New function. Uses `metadata_only` flag from manifest (already set by backup) + engine classification |
| `schema::create_ddl_objects()` | `schema::create_tables()` (schema.rs:113) | COEXIST | Same plan -- distinct restore phases | create_tables() handles Phase 2 data tables; create_ddl_objects() handles Phase 3 DDL-only objects with topo sort + retry. Different ordering and error handling requirements. |

## COEXIST Justification

1. **query_table_dependencies() / list_tables()**: Both needed because:
   - `list_tables()` is called from backup, restore, and has 2 callers
   - Adding Array(String) columns to TableRow would change its Row derive and break all callers
   - `query_table_dependencies()` is batch query returning only dep info, called once per backup
   - Cleanup: Future CH client refactor could use a single struct with optional deps

2. **create_ddl_objects() / create_tables()**: Both needed because:
   - `create_tables()` handles data tables (simple iteration, arbitrary order acceptable since data tables have no inter-dependencies)
   - `create_ddl_objects()` handles DDL-only objects with topological sort ordering and fallback retry loop
   - Different error handling: data table creation failure is fatal; DDL object creation uses retry-with-reorder fallback for CH < 23.3

## No New Public API (Modifications Only)

The following are modifications to existing code, not new public API:
- `backup::is_metadata_only_engine()`: visibility change from `fn` to `pub(crate) fn` (or duplicate in restore/topo.rs)
- `backup::create()`: internal logic change to call `query_table_dependencies()` and populate `dependencies` field
- `restore::restore()`: internal restructure to call classify + create_tables + create_ddl_objects in phases
