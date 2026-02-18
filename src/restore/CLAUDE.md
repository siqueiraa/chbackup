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

### UUID-Isolated S3 Restore (attach.rs, Phase 2c)
- S3 disk parts are restored via CopyObject instead of hardlink
- **UUID path derivation**: `store/{uuid_hex[0..3]}/{uuid_with_dashes}/{relative_path}` -- matches ClickHouse's internal S3 path convention
- `uuid_s3_prefix(uuid) -> String` generates the prefix from the destination table's UUID
- **Same-name optimization**: Before copying, calls `ListObjectsV2(prefix=store/{uuid_hex[0..3]}/{uuid_with_dashes}/)` to get existing S3 objects. Objects matching both path and size are skipped (zero-copy). Single ListObjectsV2 per table, not per-object HeadObject.
- S3 objects are copied from the backup bucket to the data bucket using `s3.copy_object_with_retry()`
- Parallel S3 copies within a table bounded by `object_disk_server_side_copy_concurrency` semaphore (default 32)
- After CopyObject: metadata files are rewritten using `object_disk::rewrite_metadata()` to update paths and set RefCount=0, ReadOnly=false
- Rewritten metadata files are written to `detached/{part_name}/` before ATTACH
- InlineData (v4+): no CopyObject needed for inline objects; preserved during rewrite
- `OwnedAttachParams` extended with S3-related fields: `s3_client`, `disk_type_map`, `object_disk_server_side_copy_concurrency`, `allow_object_disk_streaming`, `disk_remote_paths`

### Detached Path Resolution
Queries `system.tables` for `data_paths` column to find the table's data directory, then appends `detached/{part_name}/`.

### Public API
- `restore(config, ch, backup_name, table_pattern, schema_only, data_only) -> Result<()>` -- Main entry point
- `create_databases(ch, manifest) -> Result<()>` -- DDL for databases
- `create_tables(ch, manifest, filter) -> Result<()>` -- DDL for tables
- `attach_parts(params) -> Result<u64>` -- Hardlink + ATTACH PART (borrowed params), returns count
- `attach_parts_owned(params) -> Result<u64>` -- Hardlink + ATTACH PART (owned params for tokio::spawn); handles both local and S3 disk parts
- `OwnedAttachParams` -- Owned variant of AttachParams with engine field for spawn boundaries; includes `s3_client`, `disk_type_map`, `disk_remote_paths`, `object_disk_server_side_copy_concurrency`, `allow_object_disk_streaming` for Phase 2c
- `uuid_s3_prefix(uuid) -> String` -- Generate `store/{3char}/{uuid_with_dashes}/` prefix for S3 restore paths
- `detect_clickhouse_ownership(data_path) -> Result<(Option<u32>, Option<u32>)>` -- UID/GID detection
- `get_table_data_path(ch, db, table) -> Result<PathBuf>` -- Query data_paths
- `sort_parts_by_min_block(parts) -> Vec<PartInfo>` -- Sorted copy
- `needs_sequential_attach(engine) -> bool` -- Engine classification

### Parallel Restore Pattern (Phase 2a)
- Tables are restored in parallel, bounded by `effective_max_connections(config)` via a `tokio::Semaphore`
- Each `tokio::spawn` task: acquires permit -> `attach_parts_owned(OwnedAttachParams)` -> returns `(table_key, attached_count)`
- `OwnedAttachParams` uses owned types (`String`, `PathBuf`, `Vec<PartInfo>`) to cross `tokio::spawn` boundaries (no lifetime constraints)
- Engine-aware ATTACH routing: `needs_sequential_attach(engine)` returns true for Replacing/Collapsing/Versioned engines, ensuring sorted sequential ATTACH; plain MergeTree also uses sequential ATTACH within a single table (parallelism is across tables)
- `attach_parts_owned()` bridges to the internal `attach_parts_inner()` which accepts borrowed `AttachParams` -- no duplication of attach logic
- Uses `futures::future::try_join_all` for fail-fast error propagation

### Error Handling
- Uses `anyhow::Result` with `.context()` for error chain
- ATTACH PART errors for overlapping/existing parts are warnings, not failures
- Chown EPERM (not running as root) is silently skipped

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
