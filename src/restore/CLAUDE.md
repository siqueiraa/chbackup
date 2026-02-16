# CLAUDE.md -- src/restore

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `restore` command -- reads a backup manifest, creates databases/tables from DDL, hardlinks data parts to ClickHouse's `detached/` directory, and executes `ALTER TABLE ATTACH PART`.

Phase 1 implements Mode B only (non-destructive). Mode A (`--rm` DROP) is Phase 4d.

## Directory Structure

```
src/restore/
  mod.rs      -- Entry point: restore() orchestrates the full restore flow
  schema.rs   -- CREATE DATABASE and CREATE TABLE from manifest DDL
  attach.rs   -- Hardlink parts to detached/, chown, ATTACH PART
  sort.rs     -- SortPartsByMinBlock for correct attachment order
```

## Key Patterns

### Part Sort Order (sort.rs)
Parts are sorted by `(partition, min_block)` for correct merge behavior. Part name format is `{partition}_{min}_{max}_{level}` where partition may contain underscores, so parsing splits from the right. Engines containing "Replacing", "Collapsing", or "Versioned" require strictly sequential sorted ATTACH.

### Schema Creation (schema.rs)
- `create_databases()`: Executes DDL from `manifest.databases[]` with `IF NOT EXISTS` safety
- `create_tables()`: For each filtered table, checks existence first, then executes the stored `CREATE TABLE` DDL

### Part Attachment (attach.rs)
- Uses `AttachParams` struct to bundle all parameters for a table's attachment
- Hardlinks files from `{backup_dir}/shadow/{db}/{table}/{part_name}/` to `{table_data_path}/detached/{part_name}/`
- Falls back to file copy on EXDEV (cross-device error code 18)
- Chowns to ClickHouse uid/gid detected from `stat()` on data_path; skips chown if not root
- `ALTER TABLE ATTACH PART` errors 232/233 (overlapping range, already exists) are logged as warnings and skipped

### Detached Path Resolution
Queries `system.tables` for `data_paths` column to find the table's data directory, then appends `detached/{part_name}/`.

### Public API
- `restore(config, ch, backup_name, table_pattern, schema_only, data_only) -> Result<()>` -- Main entry point
- `create_databases(ch, manifest) -> Result<()>` -- DDL for databases
- `create_tables(ch, manifest, filter) -> Result<()>` -- DDL for tables
- `attach_parts(params) -> Result<u64>` -- Hardlink + ATTACH PART, returns count
- `detect_clickhouse_ownership(data_path) -> Result<(Option<u32>, Option<u32>)>` -- UID/GID detection
- `get_table_data_path(ch, db, table) -> Result<PathBuf>` -- Query data_paths
- `sort_parts_by_min_block(parts) -> Vec<PartInfo>` -- Sorted copy
- `needs_sequential_attach(engine) -> bool` -- Engine classification

### Error Handling
- Uses `anyhow::Result` with `.context()` for error chain
- ATTACH PART errors for overlapping/existing parts are warnings, not failures
- Chown EPERM (not running as root) is silently skipped

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
