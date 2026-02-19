# CLAUDE.md -- src/clickhouse

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module provides the `ChClient` wrapper around the `clickhouse-rs` crate's HTTP client. It centralizes all ClickHouse interactions: connectivity, DDL execution, FREEZE/UNFREEZE, table metadata queries, mutation checks, and part attachment.

## Directory Structure

```
src/clickhouse/
  mod.rs        -- Re-exports ChClient and related types
  client.rs     -- ChClient struct with all query methods, plus helper functions
```

## Key Patterns

### Client Wrapper Pattern
`ChClient` wraps `clickhouse::Client` (HTTP interface) with:
- Config-driven construction from `ClickHouseConfig` (host, port, username, password, secure)
- URL construction: `http(s)://{host}:{port}` (default HTTP port is 8123, NOT 9000)
- `log_sql_queries` flag: logs SQL at `info!` when true, `debug!` when false
- `log_and_execute()` internal helper for conditional SQL logging

### TLS Support (Phase 2d)
- When `secure: true`, URL scheme is `https://` (existing behavior)
- When `tls_ca` is non-empty, sets `SSL_CERT_FILE` env var for `reqwest` native-tls backend
- When `tls_cert` + `tls_key` are set, sets `SSL_CERT_FILE` and `SSL_KEY_FILE` env vars
- When `skip_verify: true`, sets `SSL_NO_VERIFY=1` env var
- Logs TLS configuration state at info level during client creation
- Note: The `clickhouse-rs` crate uses `reqwest` internally; TLS cert config is via env vars, not direct API

### Row Types
- `TableRow` -- From `system.tables`: database, name, engine, create_table_query, uuid, data_paths, total_bytes
- `MutationRow` -- From `system.mutations`: database, table, mutation_id, command, parts_to_do_names, is_done
- `MacroRow` -- From `system.macros`: macro_name (aliased from `macro`), substitution (Phase 3d)
- `DependencyRow` -- (private) From `system.tables`: database, name, dependencies_database (`Vec<String>`), dependencies_table (`Vec<String>`). Parallel arrays available in CH 23.3+. Only used by `query_table_dependencies()`.
- `DiskRow` -- From `system.disks`: name, path, type_field, remote_path
- `PartRow` -- From `system.parts`: name, partition_id, active (Phase 2d)
- `ColumnInconsistency` -- Query result: database, table, column, types (Phase 2d)
- `JsonColumnInfo` -- Query result: database, table, column, column_type (Phase 4f). Returned by `check_json_columns()` for Object/JSON type detection
- `DiskSpaceRow` -- From `system.disks`: name, path, free_space (Phase 2d)
- `NameRow` -- (private) Single `name` column, used by RBAC/named-collection/function queries (Phase 4e)
- `ShowCreateRow` -- (private) Single `statement` column, returned by `SHOW CREATE ...` queries (Phase 4e)

All use `#[derive(clickhouse::Row, serde::Deserialize, Debug, Clone)]`.

### SQL Generation Helpers
- `sanitize_name(name) -> String` -- Replaces non-alphanumeric/underscore chars with underscore
- `freeze_name(backup_name, db, table) -> String` -- Format: `chbackup_{backup}_{db}_{table}`
- `freeze_sql(db, table, freeze_name) -> String` -- ALTER TABLE FREEZE WITH NAME
- `unfreeze_sql(db, table, freeze_name) -> String` -- ALTER TABLE UNFREEZE WITH NAME
- `freeze_partition_sql(db, table, partition, freeze_name) -> String` -- ALTER TABLE FREEZE PARTITION (Phase 2d)
- `integration_table_ddl(api_host, api_port) -> (String, String)` -- Generate DDL for `system.backup_list` (URL engine -> `/api/v1/list`) and `system.backup_actions` (URL engine -> `/api/v1/actions`) (Phase 3a)
- `drop_table_sql(db, table, on_cluster) -> String` -- DROP TABLE IF EXISTS with optional ON CLUSTER clause (Phase 4d)
- `drop_database_sql(db, on_cluster) -> String` -- DROP DATABASE IF EXISTS with optional ON CLUSTER clause (Phase 4d)
- `detach_table_sync_sql(db, table) -> String` -- DETACH TABLE SYNC (Phase 4d)
- `attach_table_sql(db, table) -> String` -- ATTACH TABLE (entire table, not a part) (Phase 4d)
- `system_restore_replica_sql(db, table) -> String` -- SYSTEM RESTORE REPLICA (Phase 4d)
- `drop_replica_from_zkpath_sql(replica_name, zk_path) -> String` -- SYSTEM DROP REPLICA FROM ZKPATH (Phase 4d)
- `execute_mutation_sql(db, table, command) -> String` -- ALTER TABLE ... {command} SETTINGS mutations_sync=2 (Phase 4d)

### Public API
- `new(config) -> Result<Self>` -- Build from ClickHouseConfig (with TLS env var wiring)
- `ping() -> Result<()>` -- Connectivity check
- `inner() -> &clickhouse::Client` -- Access underlying client
- `freeze_table(db, table, freeze_name) -> Result<()>` -- ALTER TABLE FREEZE
- `freeze_partition(db, table, partition, freeze_name) -> Result<()>` -- ALTER TABLE FREEZE PARTITION (Phase 2d)
- `unfreeze_table(db, table, freeze_name) -> Result<()>` -- ALTER TABLE UNFREEZE
- `list_tables() -> Result<Vec<TableRow>>` -- Query system.tables (excludes system DBs)
- `list_all_tables() -> Result<Vec<TableRow>>` -- Query system.tables including system DBs (Phase 4f, for `tables --all` command)
- `get_table_ddl(db, table) -> Result<String>` -- SHOW CREATE TABLE
- `check_pending_mutations(targets) -> Result<Vec<MutationRow>>` -- Query system.mutations
- `query_system_parts(db, table) -> Result<Vec<PartRow>>` -- Query system.parts for active parts (Phase 2d)
- `check_parts_columns(targets) -> Result<Vec<ColumnInconsistency>>` -- Batch column consistency check (Phase 2d, design 3.3)
- `check_json_columns(targets) -> Result<Vec<JsonColumnInfo>>` -- Query `system.columns` for columns with Object or JSON types (Phase 4f, design 16.4). Follows same pattern as `check_parts_columns()`: builds IN clause from `targets`, queries with `type LIKE '%Object%' OR type LIKE '%JSON%'`. Warning-only, never blocks backup.
- `query_disk_free_space() -> Result<Vec<DiskSpaceRow>>` -- Query system.disks with free_space (Phase 2d)
- `sync_replica(db, table) -> Result<()>` -- SYSTEM SYNC REPLICA
- `attach_part(db, table, part_name) -> Result<()>` -- ALTER TABLE ATTACH PART
- `get_version() -> Result<String>` -- SELECT version()
- `get_disks() -> Result<Vec<DiskRow>>` -- Query system.disks
- `get_macros() -> Result<HashMap<String, String>>` -- Query system.macros for template resolution (Phase 3d); returns empty HashMap on error (graceful -- system.macros may not exist)
- `query_table_dependencies() -> Result<HashMap<String, Vec<String>>>` -- Query `system.tables` for `dependencies_database`/`dependencies_table` columns (CH 23.3+). Returns map from `"db.table"` to `Vec<"dep_db.dep_table">`. On query failure (CH < 23.3), catches error, logs warning, and returns `Ok(HashMap::new())` for graceful degradation. Follows `list_tables()` pattern: conditional SQL logging, `fetch_all`, `.context()`.
- `execute_ddl(ddl) -> Result<()>` -- Execute arbitrary DDL
- `create_integration_tables(api_host, api_port) -> Result<()>` -- Create `system.backup_list` and `system.backup_actions` URL engine tables for API server integration (Phase 3a)
- `drop_integration_tables() -> Result<()>` -- Drop both integration tables (called on server shutdown)
- `database_exists(db) -> Result<bool>` -- Check system.databases
- `table_exists(db, table) -> Result<bool>` -- Check system.tables
- `drop_table(db, table, on_cluster: Option<&str>) -> Result<()>` -- DROP TABLE IF EXISTS with optional ON CLUSTER, SYNC (Phase 4d, Mode A)
- `drop_database(db, on_cluster: Option<&str>) -> Result<()>` -- DROP DATABASE IF EXISTS with optional ON CLUSTER, SYNC (Phase 4d, Mode A)
- `detach_table_sync(db, table) -> Result<()>` -- DETACH TABLE SYNC (Phase 4d, ATTACH TABLE mode)
- `attach_table(db, table) -> Result<()>` -- ATTACH TABLE (entire table, not a part) (Phase 4d, ATTACH TABLE mode)
- `system_restore_replica(db, table) -> Result<()>` -- SYSTEM RESTORE REPLICA, rebuilds replica metadata from local parts (Phase 4d, ATTACH TABLE mode)
- `drop_replica_from_zkpath(replica_name, zk_path) -> Result<()>` -- SYSTEM DROP REPLICA FROM ZKPATH, removes replica from ZooKeeper (Phase 4d, ZK conflict resolution)
- `check_zk_replica_exists(zk_path, replica_name) -> Result<bool>` -- Query system.zookeeper for replica existence; returns false on query error (system.zookeeper may be unavailable) (Phase 4d, ZK conflict resolution)
- `query_database_engine(db) -> Result<String>` -- Query system.databases for engine type; returns empty string if not found (Phase 4d, DatabaseReplicated detection)
- `execute_mutation(db, table, command) -> Result<()>` -- ALTER TABLE {command} SETTINGS mutations_sync=2; waits for mutation completion (Phase 4d, mutation re-apply)
- `query_rbac_objects(entity_type: &str) -> Result<Vec<(String, String)>>` -- Query RBAC objects by entity type (USER/ROLE/ROW POLICY/SETTINGS PROFILE/QUOTA). Lists names from corresponding system table, then `SHOW CREATE {entity_type}` for each. Returns Vec of (name, DDL) tuples. Graceful degradation: returns empty Vec on query error (Phase 4e)
- `query_named_collections() -> Result<Vec<String>>` -- Query `system.named_collections` for names, then `SHOW CREATE NAMED COLLECTION` for each. Returns Vec of CREATE DDL strings. Graceful degradation on error (Phase 4e)
- `query_user_defined_functions() -> Result<Vec<String>>` -- Query `system.functions WHERE origin = 'SQLUserDefined'` for names, then `SHOW CREATE FUNCTION` for each. Returns Vec of CREATE DDL strings. Graceful degradation on error (Phase 4e)

### SQL Utility Functions
- `quote_identifier(name) -> String` -- Wraps name in backticks, escaping internal backticks by doubling them. Used by RBAC queries for safe identifier quoting (Phase 4e)

### Error Handling
- All methods return `anyhow::Result` with `.context()` annotations
- SQL queries use backtick-escaped identifiers to prevent injection
- The `clickhouse-rs` crate returns typed errors for ClickHouse server errors

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse
