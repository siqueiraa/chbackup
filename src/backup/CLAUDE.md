# CLAUDE.md -- src/backup

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `create` command -- the first step in the backup pipeline. It freezes ClickHouse tables, walks shadow directories, hardlinks data parts to a staging area, computes CRC64 checksums, and produces a `BackupManifest`.

## Directory Structure

```
src/backup/
  mod.rs          -- Entry point: create() orchestrates the full backup flow
  checksum.rs     -- CRC64 computation using crc crate (CRC_64_XZ algorithm)
  collect.rs      -- Shadow directory walk, hardlink parts to backup staging, URL encoding
  diff.rs         -- Incremental diff logic: diff_parts() compares current vs base manifest
  freeze.rs       -- FreezeGuard pattern for safe FREEZE/UNFREEZE lifecycle
  mutations.rs    -- Pre-flight pending mutation check (design 3.1)
  sync_replica.rs -- SYSTEM SYNC REPLICA for Replicated engines (design 3.2)
```

## Key Patterns

### FreezeGuard (freeze.rs)
The `FreezeGuard` tracks frozen tables and provides explicit `unfreeze_all()`. Since `Drop` is synchronous and cannot run async code, callers MUST call `unfreeze_all()` in a finally-like block. The guard accumulates `FreezeInfo` entries as tables are frozen, and iterates over them to UNFREEZE on cleanup.

### Shadow Walk and Hardlink (collect.rs)
- Uses `walkdir` via `tokio::task::spawn_blocking` to iterate shadow directories
- Shadow path structure: `{data_path}/shadow/{freeze_name}/store/{shard_hex}/{table_uuid}/{part_name}/`
- Maps shadow paths back to tables using `data_paths` from `system.tables`
- Hardlinks files from shadow to backup staging; falls back to copy on EXDEV (error code 18)
- Skips `frozen_metadata.txt` files; identifies parts by presence of `checksums.txt`

### CRC64 Checksum (checksum.rs)
- Uses `crc::Crc::<u64>::new(&crc::CRC_64_XZ)` for ClickHouse-compatible checksums
- Computes CRC64 of the `checksums.txt` file content for each part

### Incremental Diff Pattern (diff.rs)
- `diff_parts(current: &mut BackupManifest, base: &BackupManifest) -> DiffResult`: pure function (no I/O), compares parts by `(table_key, disk_name, part_name, checksum_crc64)`
- Matching parts (same name + CRC64): `source` set to `"carried:{base_name}"`, `backup_key` copied from base manifest
- CRC64 mismatch (same name, different checksum): part stays `source = "uploaded"` (re-uploaded) + `warn!()` log per design doc section 3.5
- Extra tables in base that are not in current: gracefully ignored
- `DiffResult` returns counts: `carried`, `uploaded`, `crc_mismatches`
- Triggered by `--diff-from` flag in `create()`, or by `--diff-from-remote` in `upload()` (reuses same function)

### Backup Directory Layout
```
{data_path}/backup/{backup_name}/
  metadata.json                         -- BackupManifest
  metadata/{db}/{table}.json            -- Per-table metadata
  shadow/{db}/{table}/{part_name}/...   -- Hardlinked data files
```

### Public API
- `create(config, ch, backup_name, table_pattern, schema_only, diff_from: Option<&str>) -> Result<BackupManifest>` -- Main entry point; when `diff_from` is provided, loads base manifest from local disk and applies `diff_parts()` before saving
- `diff_parts(current, base) -> DiffResult` -- Incremental comparison of current vs base manifest parts
- `compute_crc64(path) -> Result<u64>` -- File-level CRC64
- `compute_crc64_bytes(data) -> u64` -- In-memory CRC64
- `collect_parts(shadow_path, backup_dir, ...) -> Result<Vec<PartInfo>>` -- Walk and hardlink
- `freeze_table(ch, db, table, freeze_name) -> Result<()>` -- Issue FREEZE
- `check_mutations(ch, targets, timeout) -> Result<()>` -- Mutation pre-flight
- `sync_replicas(ch, tables) -> Result<()>` -- Replica sync pre-flight

### Parallel FREEZE Pattern (Phase 2a)
- Tables are frozen and collected in parallel, bounded by `effective_max_connections(config)` via a `tokio::Semaphore`
- Each `tokio::spawn` task: acquires permit -> FREEZE -> `collect_parts` (via `spawn_blocking`) -> returns `(FreezeInfo, full_name, TableManifest)`
- Uses `futures::future::try_join_all` on `JoinHandle` vec for fail-fast error propagation
- Per-task `FreezeInfo` collection: each spawned task creates its own `FreezeInfo` instead of mutating a shared `FreezeGuard`
- After all tasks join: aggregate `FreezeInfo` entries into a `FreezeGuard`, aggregate `TableManifest` entries into the manifest `HashMap`
- Error cleanup: on any task error, all successfully frozen tables are still unfrozen via the assembled `FreezeGuard`
- `ChClient` and `Arc<Vec<TableRow>>` are cloned into each spawn (both are `Clone`)

### Error Handling
- Uses `anyhow::Result` throughout with `.context()` for error chain
- `ignore_not_exists_error_during_freeze` config controls whether missing tables abort or warn
- `allow_empty_backups` config controls whether zero-table backups are errors

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
