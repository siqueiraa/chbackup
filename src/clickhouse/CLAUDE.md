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
- `DiskRow` -- From `system.disks`: name, path, type_field, remote_path
- `PartRow` -- From `system.parts`: name, partition_id, active (Phase 2d)
- `ColumnInconsistency` -- Query result: database, table, column, types (Phase 2d)
- `DiskSpaceRow` -- From `system.disks`: name, path, free_space (Phase 2d)

All use `#[derive(clickhouse::Row, serde::Deserialize, Debug, Clone)]`.

### SQL Generation Helpers
- `sanitize_name(name) -> String` -- Replaces non-alphanumeric/underscore chars with underscore
- `freeze_name(backup_name, db, table) -> String` -- Format: `chbackup_{backup}_{db}_{table}`
- `freeze_sql(db, table, freeze_name) -> String` -- ALTER TABLE FREEZE WITH NAME
- `unfreeze_sql(db, table, freeze_name) -> String` -- ALTER TABLE UNFREEZE WITH NAME
- `freeze_partition_sql(db, table, partition, freeze_name) -> String` -- ALTER TABLE FREEZE PARTITION (Phase 2d)
- `integration_table_ddl(api_host, api_port) -> (String, String)` -- Generate DDL for `system.backup_list` (URL engine -> `/api/v1/list`) and `system.backup_actions` (URL engine -> `/api/v1/actions`) (Phase 3a)

### Public API
- `new(config) -> Result<Self>` -- Build from ClickHouseConfig (with TLS env var wiring)
- `ping() -> Result<()>` -- Connectivity check
- `inner() -> &clickhouse::Client` -- Access underlying client
- `freeze_table(db, table, freeze_name) -> Result<()>` -- ALTER TABLE FREEZE
- `freeze_partition(db, table, partition, freeze_name) -> Result<()>` -- ALTER TABLE FREEZE PARTITION (Phase 2d)
- `unfreeze_table(db, table, freeze_name) -> Result<()>` -- ALTER TABLE UNFREEZE
- `list_tables() -> Result<Vec<TableRow>>` -- Query system.tables (excludes system DBs)
- `get_table_ddl(db, table) -> Result<String>` -- SHOW CREATE TABLE
- `check_pending_mutations(targets) -> Result<Vec<MutationRow>>` -- Query system.mutations
- `query_system_parts(db, table) -> Result<Vec<PartRow>>` -- Query system.parts for active parts (Phase 2d)
- `check_parts_columns(targets) -> Result<Vec<ColumnInconsistency>>` -- Batch column consistency check (Phase 2d, design 3.3)
- `query_disk_free_space() -> Result<Vec<DiskSpaceRow>>` -- Query system.disks with free_space (Phase 2d)
- `sync_replica(db, table) -> Result<()>` -- SYSTEM SYNC REPLICA
- `attach_part(db, table, part_name) -> Result<()>` -- ALTER TABLE ATTACH PART
- `get_version() -> Result<String>` -- SELECT version()
- `get_disks() -> Result<Vec<DiskRow>>` -- Query system.disks
- `get_macros() -> Result<HashMap<String, String>>` -- Query system.macros for template resolution (Phase 3d); returns empty HashMap on error (graceful -- system.macros may not exist)
- `execute_ddl(ddl) -> Result<()>` -- Execute arbitrary DDL
- `create_integration_tables(api_host, api_port) -> Result<()>` -- Create `system.backup_list` and `system.backup_actions` URL engine tables for API server integration (Phase 3a)
- `drop_integration_tables() -> Result<()>` -- Drop both integration tables (called on server shutdown)
- `database_exists(db) -> Result<bool>` -- Check system.databases
- `table_exists(db, table) -> Result<bool>` -- Check system.tables

### Error Handling
- All methods return `anyhow::Result` with `.context()` annotations
- SQL queries use backtick-escaped identifiers to prevent injection
- The `clickhouse-rs` crate returns typed errors for ClickHouse server errors

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse
