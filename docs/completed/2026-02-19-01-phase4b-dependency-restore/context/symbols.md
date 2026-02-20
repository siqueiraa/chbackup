# Symbol and Type Verification

## Existing Types Used in This Plan

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | manifest.rs:65 |
| `TableManifest.dependencies` | `Vec<String>` | `Vec<String>` | manifest.rs:116 |
| `TableManifest.metadata_only` | `bool` | `bool` | manifest.rs:112 |
| `TableManifest.engine` | `String` | `String` | manifest.rs:96 |
| `TableManifest.ddl` | `String` | `String` | manifest.rs:88 |
| `TableRow.database` | `String` | `String` | client.rs:26 |
| `TableRow.name` | `String` | `String` | client.rs:27 |
| `TableRow.engine` | `String` | `String` | client.rs:28 |
| `TableRow.create_table_query` | `String` | `String` | client.rs:29 |
| `TableRow.uuid` | `String` | `String` | client.rs:30 |
| `TableRow.data_paths` | `Vec<String>` | `Vec<String>` | client.rs:31 |
| `TableRow.total_bytes` | `Option<u64>` | `Option<u64>` | client.rs:32 |
| `BackupManifest.functions` | `Vec<String>` | `Vec<String>` | manifest.rs:73 |
| `BackupManifest.named_collections` | `Vec<String>` | `Vec<String>` | manifest.rs:77 |
| `BackupManifest.rbac` | `Option<RbacInfo>` | `Option<RbacInfo>` | manifest.rs:81 |
| `BackupManifest.databases` | `Vec<DatabaseInfo>` | `Vec<DatabaseInfo>` | manifest.rs:69 |
| `ChClient` | impl Clone | `#[derive(Clone)]` struct | client.rs:14 |

## New Types to Introduce

| Type | Kind | Location | Fields | Purpose |
|---|---|---|---|---|
| `DependencyRow` | struct (private) | client.rs | `database: String, name: String, dependencies_database: Vec<String>, dependencies_table: Vec<String>` | CH query result row for dependency info |
| `RestorePhases` | struct (pub) | restore/topo.rs | `data_tables: Vec<String>, postponed_tables: Vec<String>, ddl_only_tables: Vec<String>` | Classified table keys for phased restore |

## New Functions to Introduce

| Function | Location | Signature | Purpose |
|---|---|---|---|
| `query_table_dependencies` | client.rs | `pub async fn query_table_dependencies(&self) -> Result<HashMap<String, Vec<String>>>` | Batch query deps from system.tables (CH 23.3+) |
| `classify_restore_tables` | restore/topo.rs | `pub fn classify_restore_tables(manifest: &BackupManifest, table_keys: &[String]) -> RestorePhases` | Split tables into data/postponed/DDL-only |
| `topological_sort` | restore/topo.rs | `pub fn topological_sort(tables: &HashMap<String, TableManifest>, keys: &[String]) -> Result<Vec<String>>` | Kahn's algorithm on dependency graph |
| `engine_restore_priority` | restore/topo.rs | `pub fn engine_restore_priority(engine: &str) -> u8` | Engine priority per design 5.1 |
| `create_ddl_objects` | restore/schema.rs | `pub async fn create_ddl_objects(ch: &ChClient, manifest: &BackupManifest, ddl_keys: &[String], remap: Option<&RemapConfig>) -> Result<()>` | Phase 3 DDL creation with retry fallback |
| `create_functions` | restore/schema.rs | `pub async fn create_functions(ch: &ChClient, manifest: &BackupManifest) -> Result<()>` | Phase 4 function creation |

## ClickHouse system.tables Columns for Dependencies

From ClickHouse 23.3+ documentation:
- `dependencies_database` -- `Array(String)` -- databases of tables this object depends on
- `dependencies_table` -- `Array(String)` -- table names this object depends on

These are parallel arrays: `dependencies_database[i]` + "." + `dependencies_table[i]` gives the fully qualified name of the i-th dependency.

**Fallback for CH < 23.3**: These columns do not exist. The query must handle this gracefully (try/catch, fall back to empty deps).

## Key Design Decisions

1. **`TableManifest.dependencies` already stores `Vec<String>` in `"db.table"` format** -- no manifest schema change needed
2. **`metadata_only` is already in the manifest** -- restore-side classification uses this flag, NOT re-running engine classification
3. **Engine priority for restore ordering** per design 5.1:
   - Data tables (Phase 2): priority 0 = regular MergeTree, priority 1 = .inner tables (MV storage targets)
   - DDL-only tables (Phase 3): priority 0 = Dictionaries, priority 1 = Views/MVs, priority 2 = Distributed/Merge
4. **Fallback retry loop** for DDL objects when deps are empty: try each, collect failures, retry failures, max 10 rounds
5. **`is_metadata_only_engine()` stays private** in backup/mod.rs -- restore uses `metadata_only` flag from manifest

## Key Anti-Pattern Checks

- `TableManifest.dependencies` is `Vec<String>`, NOT `Vec<(String, String)>` -- already stores "db.table" format
- HashMap iteration order in Rust is NOT deterministic -- current restore table order is arbitrary (must fix)
- `create_tables()` takes `table_keys: &[String]` -- order IS caller's responsibility
- `query_table_dependencies()` must handle CH < 23.3 gracefully (column may not exist)
- Topological sort must handle cycles (log warning, break cycle arbitrarily)
