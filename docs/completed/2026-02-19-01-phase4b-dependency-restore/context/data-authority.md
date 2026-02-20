# Data Authority Analysis

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Table dependencies | system.tables (CH 23.3+) | `dependencies_database`, `dependencies_table` (Array(String)) | USE EXISTING - query at backup time, store in manifest |
| Engine type for ordering | system.tables | `engine` column | USE EXISTING - already in TableRow and TableManifest.engine |
| DDL for object creation | system.tables | `create_table_query` | USE EXISTING - already stored in TableManifest.ddl |
| metadata_only classification | Engine name | Derived from engine string | USE EXISTING - `is_metadata_only_engine()` already classifies |
| CH version for fallback detection | ChClient | `get_version()` already exists | USE EXISTING - compare version string |
| Functions list | BackupManifest.functions | `Vec<String>` with CREATE FUNCTION DDL | USE EXISTING - already in manifest |
| Named collections list | BackupManifest.named_collections | `Vec<String>` with DDL | USE EXISTING - already in manifest |
| RBAC metadata | BackupManifest.rbac | `Option<RbacInfo>` | USE EXISTING - already in manifest |
| Topological ordering | dependencies field in manifest | Must compute from dependency graph | MUST IMPLEMENT - no existing topo sort logic |

## Analysis Notes

1. **ClickHouse 23.3+ provides dependency info natively** via `dependencies_database` and `dependencies_table` columns in `system.tables`. These are parallel `Array(String)` columns.

2. **The manifest already has a `dependencies: Vec<String>` field** in `TableManifest` (manifest.rs:116) but it is ALWAYS set to `Vec::new()` in the backup code (backup/mod.rs:249). The field format stores `"db.table"` strings.

3. **Engine type is already captured** -- no new tracking needed. The `TableManifest.engine` field contains the engine name (e.g., "Dictionary", "MaterializedView", "View").

4. **`is_metadata_only_engine()` already exists** in `backup/mod.rs:570` and correctly classifies DDL-only engines. However, it is crate-private (`fn` not `pub fn`). Must be made public or duplicated for restore use.

5. **Functions/Named Collections/RBAC** are already represented in the manifest but their restore is Phase 4e scope. However, Phase 4b should wire the function creation since the field already exists and the DDL is already stored.

## Over-Engineering Flags

None identified. All new logic (topo sort, engine priority, fallback retry) is genuinely needed and not provided by existing data sources.
