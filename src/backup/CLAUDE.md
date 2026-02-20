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
  rbac.rs         -- RBAC, config file, named collection, and function backup (Phase 4e)
  sync_replica.rs -- SYSTEM SYNC REPLICA for Replicated engines (design 3.2)
```

## Key Patterns

### FreezeGuard (freeze.rs)
The `FreezeGuard` tracks frozen tables and provides explicit `unfreeze_all()`. Since `Drop` is synchronous and cannot run async code, callers MUST call `unfreeze_all()` in a finally-like block. The guard accumulates `FreezeInfo` entries as tables are frozen, and iterates over them to UNFREEZE on cleanup.

### Per-Disk Backup Directory (collect.rs)
- `per_disk_backup_dir(disk_path, backup_name) -> PathBuf` computes `{disk_path}/backup/{backup_name}` for any disk
- For single-disk setups where `disk_path == data_path`, this produces the same path as the legacy `{data_path}/backup/{name}` layout (zero behavior change)
- `resolve_shadow_part_path()` is the SINGLE source of truth for shadow path resolution with a 4-step fallback chain:
  1. Per-disk candidate (encoded): `{disk_path}/backup/{name}/shadow/{encoded_db}/{encoded_table}/{part}/`
  2. Legacy default (encoded): `{backup_dir}/shadow/{encoded_db}/{encoded_table}/{part}/`
  3. Legacy default (plain): `{backup_dir}/shadow/{plain_db}/{plain_table}/{part}/` (very old backups without URL encoding, skipped when plain == encoded)
  4. None (part not found at any location)
- Fallback checks **part-path existence** (not disk-path existence), correctly handling old backups with `manifest.disks` populated but legacy single-dir layout
- Used by upload (`find_part_dir`), restore (`attach_parts_inner`, `try_attach_table_mode`), and indirectly by download (write-path uses `per_disk_backup_dir` directly)
- `collect_parts()` accepts `backup_name` parameter and stages parts to `per_disk_backup_dir(disk_path, backup_name).join("shadow/...")` instead of the single `backup_dir/shadow/...`
- Logs `"staging per-disk backup dir"` per disk during collection (satisfies runtime log pattern requirement)

### Shadow Walk and Hardlink (collect.rs)
- Uses `walkdir` via `tokio::task::spawn_blocking` to iterate shadow directories
- Shadow path structure: `{data_path}/shadow/{freeze_name}/store/{shard_hex}/{table_uuid}/{part_name}/`
- Maps shadow paths back to tables using `data_paths` from `system.tables`
- Hardlinks files from shadow to backup staging; falls back to copy on EXDEV (error code 18)
- Skips `frozen_metadata.txt` files; identifies parts by presence of `checksums.txt`

### Disk-Aware Shadow Walk (collect.rs, Phase 2c)
- `collect_parts()` accepts `disk_type_map` and `disk_paths` to walk ALL disk paths, not just `data_path`
- For each shadow directory, determines the owning disk by matching against `disk_paths`
- S3 disk detection: uses `object_disk::is_s3_disk(disk_type)` to check if a disk is "s3" or "object_storage"
- For S3 disk parts: reads metadata files from shadow, calls `object_disk::parse_metadata()` to extract S3 object references, populates `PartInfo.s3_objects: Some(Vec<S3ObjectInfo>)`, skips hardlinking data files
- For local disk parts: existing hardlink behavior unchanged, `s3_objects: None`
- `CollectedPart` struct includes `disk_name: String` for proper per-disk grouping in `mod.rs`
- CRC64 checksum computed from `checksums.txt` for both local and S3 disk parts
- Part size for S3 disk parts: sum of all `ObjectRef.size` values from parsed metadata

### Projection Filtering (collect.rs, Phase 5)
- `--skip-projections` flag (CLI comma-separated) and `config.backup.skip_projections` (YAML list) control projection directory exclusion
- During `hardlink_dir()`, subdirectories ending in `.proj` are checked against the skip patterns
- Pattern matching uses `glob::Pattern` on the stem (name without `.proj` suffix): e.g., pattern `my_*` matches `my_agg.proj`
- Special value `*` skips ALL projection directories
- Uses `WalkDir::skip_current_dir()` to avoid descending into skipped projection trees (no unnecessary I/O)
- `should_skip_projection(stem, patterns)` helper performs the glob matching
- `merge_skip_projections()` in `main.rs` merges CLI flag with config list (CLI takes precedence)
- Empty pattern list means all projections are preserved (default behavior)

### Directory Size Computation (collect.rs, Phase 8)
- `pub fn dir_size(path: &Path) -> Result<u64>` -- Recursively computes the total size of all files in a directory using `walkdir`. Made public in Phase 8 (was private prior).
- Used by `backup::create()` after `backup_rbac_and_configs()` to compute `manifest.rbac_size` (from `{backup_dir}/access/`) and `manifest.config_size` (from `{backup_dir}/configs/`).
- Both sizes are logged at info level: `info!(rbac_size = ..., config_size = ..., "Computed RBAC and config sizes")`.
- Values flow into `BackupManifest.rbac_size` and `BackupManifest.config_size` (both `u64`, `#[serde(default)]` for backward compatibility), then propagate through `BackupSummary` to `ListResponse` in the server API.

### CRC64 Checksum (checksum.rs)
- Uses `crc::Crc::<u64>::new(&crc::CRC_64_XZ)` for ClickHouse-compatible checksums
- Computes CRC64 of the `checksums.txt` file content for each part

### Incremental Diff Pattern (diff.rs)
- `diff_parts(current: &mut BackupManifest, base: &BackupManifest) -> DiffResult`: pure function (no I/O), compares parts by `(table_key, disk_name, part_name, checksum_crc64)`
- Matching parts (same name + CRC64): `source` set to `"carried:{base_name}"`, `backup_key` copied from base manifest, `s3_objects` carried forward from base (Phase 2c)
- CRC64 mismatch (same name, different checksum): part stays `source = "uploaded"` (re-uploaded) + `warn!()` log per design doc section 3.5
- Extra tables in base that are not in current: gracefully ignored
- `DiffResult` returns counts: `carried`, `uploaded`, `crc_mismatches`
- Triggered by `--diff-from` flag in `create()`, or by `--diff-from-remote` in `upload()` (reuses same function)
- **S3 objects carry-forward** (Phase 2c): When a part is carried from the base manifest, `s3_objects` is cloned from the base part so the new manifest remains self-contained for download/restore. For local parts (`s3_objects: None`), cloning is a no-op.

### Backup Directory Layout
```
{data_path}/backup/{backup_name}/
  metadata.json                         -- BackupManifest (always on default disk)
  metadata/{db}/{table}.json            -- Per-table metadata
  access/users.jsonl                    -- RBAC users (Phase 4e, when --rbac)
  access/roles.jsonl                    -- RBAC roles (Phase 4e, when --rbac)
  access/row_policies.jsonl             -- RBAC row policies (Phase 4e, when --rbac)
  access/settings_profiles.jsonl        -- RBAC settings profiles (Phase 4e, when --rbac)
  access/quotas.jsonl                   -- RBAC quotas (Phase 4e, when --rbac)
  configs/...                           -- ClickHouse config files (Phase 4e, when --configs)

# Per-disk shadow directories (hardlinked data files):
{disk_path}/backup/{backup_name}/shadow/{db}/{table}/{part_name}/...
# When disk_path == data_path (single-disk), this is inside the default backup dir.
# When disk_path != data_path (multi-disk), this is on the same filesystem as the source.
```

### Partition-Level Freeze (Phase 2d)
- When `--partitions` flag is set, `create()` calls `ch.freeze_partition(db, table, partition, freeze_name)` for each comma-separated partition ID instead of `ch.freeze_table()`
- Partition IDs are parsed from the comma-separated `--partitions` string and trimmed
- Multiple partitions are frozen sequentially within a single table task (partition-level parallelism not needed)
- The freeze_name is the same regardless of whether whole-table or per-partition
- Shadow walk proceeds identically (frozen parts end up in same shadow directory)

### Disk Filtering (Phase 2d)
- Before processing collected parts, each part is checked against `config.clickhouse.skip_disks` and `config.clickhouse.skip_disk_types`
- Uses `table_filter::is_disk_excluded(disk_name, disk_type, skip_disks, skip_disk_types)` for exclusion check
- Excluded parts are logged at info level and skipped from the backup

### Parts Column Consistency Check (Phase 2d)
- After listing tables, if `config.clickhouse.check_parts_columns` is true AND `!skip_check_parts_columns` CLI flag:
  - Builds `Vec<(String, String)>` of (database, table) pairs from filtered tables
  - Calls `ch.check_parts_columns(&targets)` to find column type inconsistencies
  - Filters out benign drift: types containing "Enum", "Tuple", "Nullable", or "Array(Tuple"
  - Remaining inconsistencies are logged as warnings per table/column
- The check runs BEFORE FREEZE to avoid wasting time on tables that will fail on restore

### JSON/Object Column Detection (Phase 4f, design 16.4)
- After the parts column consistency check, backup pre-flight calls `ch.check_json_columns(&targets)` to detect columns with Object or JSON types
- Warning-only: never blocks the backup, only logs warnings per column and an aggregate info message
- Follows the same try/match pattern as `check_parts_columns`: `Ok(json_cols)` -> log warnings per column, `Err(e)` -> warn and continue
- Uses the same `targets` Vec<(String, String)> already built for the parts column check
- No config gate -- always runs (zero-cost query)

### RBAC, Config, Named Collections, and Functions Backup (rbac.rs, Phase 4e)
- `backup_rbac_and_configs(config, ch, backup_dir, manifest, rbac, configs, named_collections) -> Result<()>` -- Orchestrates all Phase 4e backup subsystems. Called after manifest creation but before the diff step. Each subsystem is gated by its CLI flag OR the corresponding `*_backup_always` config value.
- **RBAC backup** (`backup_rbac()`): Queries `ch.query_rbac_objects(entity_type)` for each of 5 entity types (USER, ROLE, ROW POLICY, SETTINGS PROFILE, QUOTA). Serializes results as JSONL files to `{backup_dir}/access/{entity_type}.jsonl`. Each line is a JSON object with `entity_type`, `name`, `create_statement` fields. Sets `manifest.rbac = Some(RbacInfo { path: "access/" })`.
- **Config backup** (`backup_configs()`): Uses `spawn_blocking` + `walkdir` to copy all files from `config.clickhouse.config_dir` to `{backup_dir}/configs/`, preserving directory structure. Skips with warning if config dir does not exist.
- **Named collections backup** (`backup_named_collections()`): Calls `ch.query_named_collections()` to get Vec of CREATE DDL strings. Stores directly in `manifest.named_collections`.
- **Functions backup** (`backup_functions()`): Calls `ch.query_user_defined_functions()` to get Vec of CREATE DDL strings. Stores in `manifest.functions`. Always runs regardless of flags (zero-cost DDL in manifest). This completes the round-trip: backup captures functions, restore recreates them (previously `manifest.functions` was always empty during backup).
- `RbacEntry` struct (private): `entity_type`, `name`, `create_statement` -- serialized to JSONL format.
- `RBAC_ENTITY_TYPES` constant: Maps SQL entity types to lowercase identifiers and JSONL filenames.

### Public API
- `create(config, ch, backup_name, table_pattern, schema_only, diff_from: Option<&str>, partitions: Option<&str>, skip_check_parts_columns: bool, rbac: bool, configs: bool, named_collections: bool, skip_projections: &[String]) -> Result<BackupManifest>` -- Main entry point; supports partition-level freeze, parts column check (Phase 2d), RBAC/config/named-collections backup (Phase 4e), and projection filtering (Phase 5)
- `diff_parts(current, base) -> DiffResult` -- Incremental comparison of current vs base manifest parts
- `compute_crc64(path) -> Result<u64>` -- File-level CRC64
- `compute_crc64_bytes(data) -> u64` -- In-memory CRC64
- `per_disk_backup_dir(disk_path, backup_name) -> PathBuf` -- Compute per-disk backup directory `{disk_path}/backup/{backup_name}`
- `resolve_shadow_part_path(backup_dir, manifest_disks, backup_name, disk_name, encoded_db, encoded_table, plain_db, plain_table, part_name) -> Option<PathBuf>` -- 4-step fallback chain for shadow path resolution (per-disk -> legacy encoded -> legacy plain -> None)
- `collect_parts(data_path, freeze_name, backup_dir, backup_name, tables, disk_type_map, disk_paths, skip_projections: &[String]) -> Result<HashMap<String, Vec<CollectedPart>>>` -- Walk all disk shadow directories, stage to per-disk backup dirs, detect S3 disk parts, hardlink local parts, filter projections (Phase 2c + Phase 5 + per-disk updated signature)
- `CollectedPart` -- Struct with `database`, `table`, `part_info: PartInfo`, `disk_name: String`
- `freeze_table(ch, db, table, freeze_name) -> Result<()>` -- Issue FREEZE
- `check_mutations(ch, targets, timeout) -> Result<()>` -- Mutation pre-flight
- `sync_replicas(ch, tables) -> Result<()>` -- Replica sync pre-flight

### Dependency Population (Phase 4b)
- After `list_tables()`, calls `ch.query_table_dependencies()` to get a `HashMap<String, Vec<String>>` mapping `"db.table"` to its dependencies
- On query failure (CH < 23.3), falls back to empty map with a warning (dependencies will be `Vec::new()`)
- Logs `tables_with_deps` count at info level
- For metadata-only tables: looks up `deps_map.get(&full_name).cloned().unwrap_or_default()` directly
- For data tables inside `tokio::spawn`: wraps `deps_map` in `Arc<HashMap>` (`deps_arc`), clones `Arc` into each spawn, then looks up `deps_clone.get(&full_name).cloned().unwrap_or_default()`
- This populates `TableManifest.dependencies` which was previously always `Vec::new()`
- Dependencies are serialized in the manifest and consumed by `restore/topo.rs` for topological sort

### Parallel FREEZE Pattern (Phase 2a)
- Tables are frozen and collected in parallel, bounded by `effective_max_connections(config)` via a `tokio::Semaphore`
- Each `tokio::spawn` task: acquires permit -> FREEZE -> `collect_parts` (via `spawn_blocking`) -> returns `(FreezeInfo, full_name, TableManifest)`
- Uses `futures::future::try_join_all` on `JoinHandle` vec for fail-fast error propagation
- Per-task `FreezeInfo` collection: each spawned task creates its own `FreezeInfo` instead of mutating a shared `FreezeGuard`
- After all tasks join: aggregate `FreezeInfo` entries into a `FreezeGuard`, aggregate `TableManifest` entries into the manifest `HashMap`
- Error cleanup: on any task error, all successfully frozen tables are still unfrozen via the assembled `FreezeGuard`
- `ChClient` and `Arc<Vec<TableRow>>` are cloned into each spawn (both are `Clone`)

### Per-Disk Error Cleanup (mod.rs)
- On `backup::create()` failure, `cleanup_failed_backup()` removes both the default backup directory AND all per-disk backup directories
- Uses `std::fs::canonicalize()` + `HashSet` dedup to prevent double-delete when paths resolve to the same directory (e.g., symlinks)
- Per-disk dir cleanup is non-fatal (warn on failure); default backup_dir cleanup follows existing error handling
- Disk map (`HashMap<String, String>`) from `ch.get_disks()` is already in scope at the error cleanup site

### Error Handling
- Uses `anyhow::Result` throughout with `.context()` for error chain
- `ignore_not_exists_error_during_freeze` config controls whether missing tables abort or warn
- `allow_empty_backups` config controls whether zero-table backups are errors

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
