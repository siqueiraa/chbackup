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

### Row Types
- `TableRow` -- From `system.tables`: database, name, engine, create_table_query, uuid, data_paths, total_bytes
- `MutationRow` -- From `system.mutations`: database, table, mutation_id, command, parts_to_do_names, is_done
- `DiskRow` -- From `system.disks`: name, path, type_field

All use `#[derive(clickhouse::Row, serde::Deserialize, Debug, Clone)]`.

### SQL Generation Helpers
- `sanitize_name(name) -> String` -- Replaces non-alphanumeric/underscore chars with underscore
- `freeze_name(backup_name, db, table) -> String` -- Format: `chbackup_{backup}_{db}_{table}`
- `freeze_sql(db, table, freeze_name) -> String` -- ALTER TABLE FREEZE WITH NAME
- `unfreeze_sql(db, table, freeze_name) -> String` -- ALTER TABLE UNFREEZE WITH NAME

### Public API
- `new(config) -> Result<Self>` -- Build from ClickHouseConfig
- `ping() -> Result<()>` -- Connectivity check
- `inner() -> &clickhouse::Client` -- Access underlying client
- `freeze_table(db, table, freeze_name) -> Result<()>` -- ALTER TABLE FREEZE
- `unfreeze_table(db, table, freeze_name) -> Result<()>` -- ALTER TABLE UNFREEZE
- `list_tables() -> Result<Vec<TableRow>>` -- Query system.tables (excludes system DBs)
- `get_table_ddl(db, table) -> Result<String>` -- SHOW CREATE TABLE
- `check_pending_mutations(targets) -> Result<Vec<MutationRow>>` -- Query system.mutations
- `sync_replica(db, table) -> Result<()>` -- SYSTEM SYNC REPLICA
- `attach_part(db, table, part_name) -> Result<()>` -- ALTER TABLE ATTACH PART
- `get_version() -> Result<String>` -- SELECT version()
- `get_disks() -> Result<Vec<DiskRow>>` -- Query system.disks
- `execute_ddl(ddl) -> Result<()>` -- Execute arbitrary DDL
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
