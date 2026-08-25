# CLAUDE.md -- src/backup

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `create` command -- the first step in the backup pipeline. It freezes ClickHouse tables, walks shadow directories, hardlinks data parts to a staging area, computes CRC64 checksums, and produces a `BackupManifest`.

## Directory Structure

```
src/backup/
  mod.rs          -- Entry point: create() orchestrates the full backup flow
  checksum.rs     -- CRC64 computation using crc crate (CRC_64_XZ algorithm)
  collect.rs      -- Shadow directory walk, hardlink parts to backup staging
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

### Path Encoding (collect.rs)
- `url_encode_path()` has been removed; all call sites now use `crate::path_encoding::encode_path_component()` which provides a canonical, DRY percent-encoding implementation with byte-level multi-byte UTF-8 handling

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
- `S3ObjectInfo.path` is normalised to a **disk-relative** key via `object_disk::disk_relative_key(stored_key, source_key_prefix)`. This matters because v5 metadata stores the *complete* object key (prefix included) while v2-v4 store the relative one; recording one form lets upload rebuild the source with `upload_source_key()` and restore re-spell it per version with `restore_object_keys()`
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
- The projections **actually stripped** (not the patterns requested) are recorded in `manifest.stripped_projections`. Restore reads that field to decide whether the target server can tolerate the resulting parts -- see `src/restore/CLAUDE.md`. An empty list therefore has to mean "nothing was stripped", so do not populate it from the pattern list

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
- Triggered by `--diff-from` flag in `create()`, `--diff-from-remote` in `create()` (downloads remote manifest from S3), or `--diff-from-remote` in `upload()` (reuses same function)
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
- When `--partitions` is set, `create()` calls `ch.freeze_partition(db, table, partition_id, freeze_name)` for each ID instead of `ch.freeze_table()`, emitting `ALTER TABLE ... FREEZE PARTITION ID '<id>'`
- Values are literal `system.parts.partition_id` strings, NOT partition key expressions. The ID form is mandatory: the two only coincide for single-column numeric keys (`toYYYYMM` -> `202401`), and diverge for unpartitioned tables (`all` vs `tuple()`), multi-column keys (`2024-29` vs `(2024, 29)`), and `String` keys (16-hex hash). Sending the expression form yields error 248 INVALID_PARTITION_VALUE
- `parse_partition_list()` returns a tri-state `PartitionSpec`:
  - `Unspecified` -- no flag (or only whitespace/empty entries); `clickhouse.freeze_by_part` discovery may still apply
  - `WholeTable` -- `--partitions all` alone; takes the whole-table FREEZE path and deliberately does NOT fall through to discovery, otherwise the explicit flag would be silently overridden by config
  - `Ids(..)` -- explicit list, with `"all"` retained as a valid ID (mixed lists like `all,202401` freeze exactly the listed set; on a partitioned table `all` errors and is swallowed only under the default `ignore_not_exists_error_during_freeze: true`)
- Multiple partitions are frozen sequentially within a single table task (partition-level parallelism not needed)
- The freeze_name is the same regardless of whether whole-table or per-partition
- Shadow walk proceeds identically (frozen parts end up in same shadow directory)
- **Error 218 is `TABLE_IS_DROPPED`.** There is no `CANNOT_FREEZE_PARTITION` code in ClickHouse; the codes are traced to `ErrorCodes.cpp` and are identical across the four CI matrix versions. `is_ignorable_freeze_error()` groups 218 with `UNKNOWN_TABLE` (60) and `UNKNOWN_DATABASE` (81), all ignorable only under `ignore_not_exists_error_during_freeze`. Ignoring it is not the same as it being harmless: the table was dropped mid-backup and will be absent from the manifest. Code 248 `INVALID_PARTITION_VALUE` is deliberately **not** in that set

### Freeze Evidence Check (freeze.rs + mod.rs)
A successful `ALTER TABLE ... FREEZE PARTITION` is **not** evidence that anything was frozen: ClickHouse's `MergeTreeData::freezePartitionsByMatcher` returns success with an empty result when the matcher selects no partition. Trusting the SQL result turns a mistyped partition ID into a quietly partial backup, so per-partition freezes are judged by what actually landed on disk.

- `partitions_with_shadow_evidence(disk_paths, freeze_name, requested) -> HashSet<String>` walks `{disk_path}/shadow/{freeze_name}` on every disk and returns which requested IDs have at least one part staged. Part directories sit at depth four in both shadow layouts (`data/{db}/{table}/{part}` and `store/{3char}/{uuid}/{part}`) and a part name always begins with `{partition_id}_`. Blocking I/O -- `create()` calls it inside `spawn_blocking`.
- `freeze_evidence_outcome(partition_id, evidence_present, explicitly_requested) -> FreezeOutcome` decides what a zero-match means, and the two cases differ deliberately:
  - `FailExplicitZeroMatch` -- an operator-supplied `--partitions` ID staged nothing. **Hard error**, naming the partition and the table and pointing at `system.parts.partition_id`. That is a typo, and continuing would publish a backup silently missing the requested data.
  - `WarnDiscoveryZeroMatch` -- an ID discovered from `system.parts` staged nothing. Non-fatal: it may legitimately have been merged away between the query and the FREEZE.
- When discovery produced IDs but *none* could be frozen, a further `warn!` fires -- the table would otherwise be dropped from the manifest with only an `info!`, a silent data gap

### Disk Filtering (Phase 2d)
- Applied at **whole-disk granularity during the shadow walk**, NOT per part: `collect_parts` checks each disk once (`collect.rs`, inside the per-disk loop) and `continue`s past excluded disks entirely
- Uses `table_filter::is_disk_excluded(disk_name, disk_type, skip_disks, skip_disk_types)` for the exclusion check
- Excluded disks are logged at info level; every part on them is absent from the backup
- **Only `backup::create` consults these settings.** Upload, download, and restore act on whatever `manifest.disk_types` contains, so excluding a disk silently omits its tables' data rather than failing. `restore/mod.rs` acknowledges the fallout when reporting tables with zero parts.

### S3 Object-Disk Durability Contract
- **S3 object-disk parts are pointers, not data.** `collect_s3_part_metadata` records `S3ObjectInfo` references and stages only the local *metadata* files; the referenced remote objects are never copied or pinned at create time.
- The staged shadow metadata hardlinks are ClickHouse's **only refcount** on those remote objects. Releasing the FREEZE lets ClickHouse merge the parts away and garbage-collect the objects.
- Therefore a table with S3-disk parts must stay frozen until `upload` has run its CopyObject. `create()` takes `defer_unfreeze_s3` and, when set, splits the `FreezeGuard` so only those tables remain frozen; ownership is recorded in `deferred.rs` and released by `upload`'s in-task finaliser.
- Local-disk tables need no deferral: `collect_parts` hardlinked their data into the backup dir, so merges cannot destroy it.
- **Every built-in path defers**, including standalone `create` and the API `create` branches. The k8s CronJob pattern enqueues `create` and `upload` as *separate* commands, so restricting deferral to `create_remote` left the production path racing. The persisted record (`deferred.rs`) is what makes the cross-command handoff work: `upload` loads it, releases the freeze after CopyObject, and an expired orphan is reaped at the next `create`.
- **`protection_status()` uses three independent signals** — per-backup lock active, recorded owner process alive, TTL unexpired — and any one protects. That layering is why no `PidLock` handoff into the upload task is needed: cancellation drops the caller's lock, but in server mode the owner (the server) is still alive, and cross-process the TTL still holds. A lock-only predicate would need the guard threaded into `upload`, which `run_operation` cannot do (generic over a closure) and which `create_remote` would break (it holds the lock across both phases).
- **Owner liveness is identity-checked, not a PID existence check.** A PID is not an identity: in a container the entrypoint is always PID 1, so a record written by a process that has since been replaced still "exists". `owner_boot_token` pairs the PID with its `/proc/<pid>/stat` start time. Absent token (legacy record, or no `/proc`) ⇒ **not live**, leaving the TTL as the bound — falling back to PID-only there is what kept pre-upgrade records protected forever, since `protection_status` answers on liveness before it reaches the TTL branch. Unreadable current token ⇒ **live** (fail closed). `owned_by_current_process` is token-aware too, and is checked *first* by `load()`, so bare PID equality would let a fresh PID 1 claim a dead pod's record and skip both liveness and the TTL. Note the deliberate asymmetry: for ownership the fail-closed answer is "not mine", for liveness "still alive" — both err toward keeping the freeze protected.
- **Owner identity means "an operation is in flight", not "a process exists".** The token alone does not fix a long-lived server: the process that wrote the record is still running and its token still matches, so liveness protects it forever and the TTL is unreachable. So whoever *ends* an operation while leaving a record in place must **demote** it (`demote()` — owner cleared, `retained`/`created_at_secs`/`ttl_secs` preserved). Two enforcement points cover every path: `finalize_deferred_freeze`'s `!upload_succeeded` branch, and `retain_failed()` itself, which is only ever reached as an operation ends and therefore demotes unconditionally — covering the successful-upload-with-partial-unfreeze, reaper-partial-failure, and create-post-freeze-cleanup callers at once.
- **`created_at_secs` is never advanced.** Neither `adopt()` nor demotion touches it, so the TTL bounds the age of the *freeze*, not of its latest owner. Advancing it would let N pod restarts grant N × TTL. Protection during an adopting operation comes from the per-backup lock it holds, which is the strongest signal anyway.
- **`retain_failed()` carries the prior record's `ttl_secs` forward.** It used to rebuild from the `DEFAULT_TTL_SECS` constant, silently resetting a raised TTL to 24h after any partial unfreeze and reintroducing the `NoSuchKey` race. The published value is used rather than current config, so a config change mid-flight cannot retroactively alter a hold already in place.
- **The record is KEPT when an upload fails.** `upload.state.json` survives, so a retry needs the objects still pinned; releasing on failure made every retry unprotected. It is demoted, not released, so the TTL still bounds it.
- **`delete_local` refuses while a freeze is held** — the backup dir contains the record, so deleting it would strand the freeze. This matters most on the retention path, which runs automatically after every successful upload.
- **Reaping is effective only for callers that do not hold `Global`.** `reap_expired` acquires a `Backup(name)` lock per candidate, and the tiers are mutually exclusive, so the two routes reachable from `clean` (`main.rs` CLI and the API route, both via `list::clean_shadow`) reap **nothing** — a wholly skipped pass is logged at `debug`. The two that work are `create` pre-flight (a direct call) and `cleanup_failed_backup` (via `clean_shadow_force`). `chbackup release-deferred <name>` is the operator escape hatch; it is deliberately not a `clean` flag, because `clean` could never acquire the lock a release needs.
- **KNOWN LIMITATION: ownership is process-level, not operation-level.** The record identifies an
  owning *process* (PID + start-time token), so two operations in the *same* process are
  indistinguishable to it. That matters on the cancellation path: cancelling an operation drops the
  caller's future and its PID lock while `upload`'s spawned task keeps copying, detached
  (`upload/mod.rs:160-163`). Three consequences, all pre-dating the token/demotion work and none
  fixed by it:
  1. The detached task runs without the per-backup lock, violating `load`'s documented precondition
     that callers hold it so the adopt decision is serialised (`deferred.rs:277-280`).
  2. A retry started after cancellation sees the record as *already owned* (same process), so the
     **old** detached finaliser can unfreeze and delete a record the retry is still copying against.
  3. `load` on publish failure returns adopted ownership while the on-disk record stays unclaimed
     (`deferred.rs:318-324`), so a claim can silently be in-memory only.
  Closing these needs an operation-instance identity in the record (an op id, not just a PID) plus
  lock retention into the detached task. That is a design change, deliberately not attempted as a
  patch. Until then the TTL is the backstop, and it must exceed the worst-case upload duration.
- **`own_lock` makes the same-name case work.** The reaper's pre-check would otherwise see the *caller's own* lock and skip — which is why `create`'s documented same-name coverage never actually did anything. Callers holding a backup's lock pass its name; that candidate is judged with `status_under_lock` and not re-acquired. The same helper fixes the post-acquisition recheck, which used to read the lock the reaper had just written and conclude it was protected, so `reap_expired` could never reap at all.

### Parts Column Consistency Check (Phase 2d)
- After listing tables, if `config.clickhouse.check_parts_columns` is true AND `!skip_check_parts_columns` CLI flag:
  - Builds `Vec<(String, String)>` of (database, table) pairs from filtered tables
  - Calls `ch.check_parts_columns(&targets)` to find column type inconsistencies
  - Filters out benign drift: types containing "Enum", "Tuple", "Nullable", or "Array(Tuple"
  - Remaining (actionable) inconsistencies cause the backup to fail with `bail!()` (strict-fail). Use `--skip-check-parts-columns` to bypass.
  - Query-level errors (e.g., ClickHouse unreachable) remain warn-only (do not block backup)
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
- `create(config, ch, backup_name, table_pattern, schema_only, diff_from: Option<&str>, diff_from_remote: Option<&str>, s3: Option<&S3Client>, partitions: Option<&str>, skip_check_parts_columns: bool, rbac: bool, configs: bool, named_collections: bool, skip_projections: &[String]) -> Result<BackupManifest>` -- Main entry point; supports partition-level freeze, parts column check (Phase 2d), RBAC/config/named-collections backup (Phase 4e), projection filtering (Phase 5), and remote incremental base via `--diff-from-remote` (downloads manifest from S3, skips hardlinks for matching parts)
- `diff_parts(current, base) -> DiffResult` -- Incremental comparison of current vs base manifest parts
- `compute_crc64(path) -> Result<u64>` -- File-level CRC64
- `compute_crc64_bytes(data) -> u64` -- In-memory CRC64
- `per_disk_backup_dir(disk_path, backup_name) -> PathBuf` -- Compute per-disk backup directory `{disk_path}/backup/{backup_name}`
- `resolve_shadow_part_path(backup_dir, manifest_disks, backup_name, disk_name, encoded_db, encoded_table, plain_db, plain_table, part_name) -> Option<PathBuf>` -- 4-step fallback chain for shadow path resolution (per-disk -> legacy encoded -> legacy plain -> None)
- `collect_parts(data_path, freeze_name, backup_name, tables, disk_type_map, disk_paths, skip_disks, skip_disk_types, skip_projections: &[String], base_parts: Option<&BasePartsMap>) -> Result<HashMap<String, Vec<CollectedPart>>>` -- Walk all disk shadow directories, stage to per-disk backup dirs, detect S3 disk parts, hardlink local parts, filter projections, skip hardlinks for parts matching remote base by CRC64 (Phase 2c + Phase 5 + per-disk + diff-from-remote)
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
