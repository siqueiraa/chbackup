# Plan: Phase 3c -- Retention / GC

## Goal

Implement local retention, remote retention with safe GC, the `clean` command (shadow directory cleanup), wire the CLI and API endpoints, and resolve the dual retention config (`general.*` vs `retention.*`).

## Architecture Overview

All retention and cleanup logic lives in `src/list.rs`, following the existing pattern established by `clean_broken_local()` / `clean_broken_remote()`. The plan adds six public functions to `list.rs`, wires the `clean` command in `main.rs`, replaces the `clean_stub` in `routes.rs`, and adds a config resolution helper. No new files or types are created -- only new functions in existing modules.

**Key design references:** design doc sections 8.1, 8.2, 8.3, 8.4, 13.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Retention logic**: Owned by `list.rs` -- centralized listing, filtering, and deletion
- **API routes**: Owned by `server/routes.rs` -- HTTP handlers following existing pattern
- **CLI dispatch**: Owned by `main.rs` -- command dispatch following existing `CleanBroken` pattern
- **Config**: Owned by `config.rs` -- RetentionConfig already has `backups_to_keep_local` (i32) and `backups_to_keep_remote` (i32) at config.rs:378-386
- **Locking**: Owned by `lock.rs` -- `lock_for_command("clean", ...)` already returns `LockScope::Global`

### What This Plan CANNOT Do
- Cannot add manifest caching (design 8.2 mentions server-mode caching -- deferred to future optimization)
- Cannot test GC with real S3 in unit tests (would be integration test only)
- Cannot parallelize retention across different hosts (design doc acknowledges multi-host race as a known edge case)
- Cannot implement `backups_to_keep_local: -1` delete-after-upload semantics in retention -- that is handled by the `upload` module's `delete_local` flag, not by retention. The retention function only handles positive counts.

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| GC deletes keys still referenced by another backup | GREEN | Build referenced key set from ALL surviving manifests before deleting; re-check for race (design 8.2 step 3c) |
| Retention deletes broken backup mistakenly | GREEN | Broken backups (timestamp=None, is_broken=true) are excluded from retention counting and deletion |
| Shadow cleanup removes non-chbackup shadow dirs | GREEN | Only removes entries matching `chbackup_*` prefix (design 13) |
| Config resolution between general.* and retention.* | GREEN | Simple rule: retention.* overrides general.* when non-zero |
| GC performance with many remote backups | YELLOW | Initial implementation loads all manifests each cycle. Deferred caching for optimization. Manifests are small JSON files -- acceptable for up to ~100 backups. |
| Concurrent create_remote on another host during GC | YELLOW | Design acknowledges this race (millisecond window). Mitigated by PID lock on same host; multi-host users should run retention from one host only. |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `retention_local: deleted N of M local backups` | yes | Confirms local retention ran and how many backups were deleted |
| `retention_remote: deleted N of M remote backups` | yes | Confirms remote retention ran with GC |
| `gc: collected N referenced keys from M manifests` | yes | Confirms GC key collection worked |
| `gc: deleting N unreferenced keys, preserving N referenced` | yes | Confirms GC filtering worked |
| `clean_shadow: removed N shadow directories` | yes | Confirms shadow cleanup ran |
| `ERROR:.*retention` | no (forbidden) | Errors during retention should be warnings per-item, not fatal |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Manifest caching in server mode | Performance optimization, not correctness | Future plan when backup count exceeds ~100 |
| Remote retention called automatically after upload | Requires watch mode integration | Phase 3d (watch) |
| `backups_to_keep_local: -1` (delete after upload) | Already implemented in upload module via `delete_local` flag | N/A -- done |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: Config resolution helpers + retention_local
  - Task 2: GC key collection (gc_collect_referenced_keys)
  - Task 3: GC-safe deletion + retention_remote (depends on Task 2)

Group B (CLI + API Wiring -- depends on Group A):
  - Task 4: Wire CLI clean command + retention commands
  - Task 5: Replace clean_stub with real API handler

Group C (Clean Command -- Independent of Group A):
  - Task 6: Shadow directory cleanup (clean_shadow)

Group D (Documentation -- depends on all above):
  - Task 7: Update CLAUDE.md for all modified modules
```

## Tasks

### Task 1: Config resolution helpers + local retention

**TDD Steps:**

1. **Write failing test:** `test_effective_retention_local`
   - Create a Config with `retention.backups_to_keep_local = 3` and `general.backups_to_keep_local = 5`
   - Assert `effective_retention_local(&config)` returns `3` (retention overrides general)
   - Create a Config with `retention.backups_to_keep_local = 0` and `general.backups_to_keep_local = 5`
   - Assert `effective_retention_local(&config)` returns `5` (fallback to general when retention is 0)
   - Same tests for `effective_retention_remote()`

2. **Write failing test:** `test_retention_local_deletes_oldest`
   - Create a temp dir with 5 valid backups (with timestamps spread across 5 days)
   - Call `retention_local(data_path, 3)` (keep 3)
   - Assert returns `Ok(2)` (deleted 2)
   - Assert the 3 newest backups remain, 2 oldest are gone

3. **Write failing test:** `test_retention_local_skips_broken`
   - Create a temp dir with 4 valid backups + 1 broken backup
   - Call `retention_local(data_path, 3)` (keep 3)
   - Assert returns `Ok(1)` (deleted 1 of the valid ones, the oldest)
   - Assert the broken backup is untouched (still exists)

4. **Write failing test:** `test_retention_local_zero_means_unlimited`
   - Create 5 backups
   - Call `retention_local(data_path, 0)` (unlimited)
   - Assert returns `Ok(0)` (deleted none)

5. **Implement** in `src/list.rs`:
   - `effective_retention_local(config: &Config) -> i32` -- returns `retention.backups_to_keep_local` when non-zero, else `general.backups_to_keep_local`
   - `effective_retention_remote(config: &Config) -> i32` -- returns `retention.backups_to_keep_remote` when non-zero, else `general.backups_to_keep_remote`
   - `retention_local(data_path: &str, keep: i32) -> Result<usize>` following the `clean_broken_local` pattern:
     - `keep == 0` or `keep == -1` => return Ok(0) (no retention action; -1 is upload module's concern)
     - Call `list_local(data_path)?`
     - Filter out broken backups (is_broken == true)
     - Sort valid backups by timestamp (ascending, None timestamps treated as very old)
     - If valid count <= keep => return Ok(0)
     - Delete the oldest (valid_count - keep) backups via `delete_local()`
     - Return count of deleted backups

6. **Verify tests pass**
7. **Add import:** `use crate::config::Config;` to list.rs

**Files:** `src/list.rs`
**Acceptance:** F001

**Implementation Notes:**
- Follow the `clean_broken_local` pattern: list -> filter -> delete loop -> return count
- Errors on individual deletes are logged as warnings, not fatal (pattern from `clean_broken_local`)
- `effective_retention_*` functions need `use crate::config::Config;`

---

### Task 2: GC referenced key collection

**TDD Steps:**

1. **Write unit test:** `test_collect_referenced_keys_from_manifest`
   - Create a `BackupManifest` with known `backup_key` values in parts and `s3_objects`
   - Call a helper `collect_keys_from_manifest(&manifest) -> HashSet<String>`
   - Assert all expected keys are present

2. **Implement** in `src/list.rs`:
   - `collect_keys_from_manifest(manifest: &BackupManifest) -> HashSet<String>` (private helper):
     - Iterate `manifest.tables.values()` -> `table.parts.values()` -> each `Vec<PartInfo>`
     - For each `PartInfo`: add `part.backup_key` to set
     - For each `PartInfo.s3_objects` (if Some): add each `s3_obj.backup_key` to set
     - Return the set
   - `gc_collect_referenced_keys(s3: &S3Client, exclude_backup: &str) -> Result<HashSet<String>>` (public):
     - Call `list_remote(s3).await?` to get all remote backups
     - Filter out the backup being deleted (`exclude_backup`)
     - Filter out broken backups (is_broken == true)
     - For each surviving backup: `s3.get_object(&format!("{}/metadata.json", name)).await?` -> `BackupManifest::from_json_bytes(&data)?`
     - Collect all keys via `collect_keys_from_manifest()` into a union HashSet
     - Log count: `info!(manifest_count = ..., key_count = ..., "gc: collected N referenced keys from M manifests")`
     - Return the set

3. **Verify tests pass**

**Files:** `src/list.rs`
**Acceptance:** F002

**Implementation Notes:**
- The unit test can only test `collect_keys_from_manifest()` (no S3 in unit tests)
- `gc_collect_referenced_keys()` is integration-test-only (needs real S3)
- Add `use std::collections::HashSet;` at top of list.rs (already used in tests module but not in main module)

---

### Task 3: GC-safe deletion + remote retention

**TDD Steps:**

1. **Write unit test:** `test_gc_filter_unreferenced_keys`
   - Create a list of S3 keys: `["a/data/part1.tar.lz4", "a/data/part2.tar.lz4", "a/metadata.json"]`
   - Create a referenced set: `{"a/data/part1.tar.lz4"}` (part1 is referenced by another backup)
   - Verify the filtering logic: only `"a/data/part2.tar.lz4"` and `"a/metadata.json"` should be candidates
   - But metadata.json is deleted last, so the immediate candidates are just `["a/data/part2.tar.lz4"]`

2. **Implement** in `src/list.rs`:
   - `gc_delete_backup(s3: &S3Client, backup_name: &str, referenced_keys: &HashSet<String>) -> Result<()>` (public):
     - List all S3 keys under `{backup_name}/` prefix via `s3.list_objects(&prefix).await?`
     - **Key format conversion**: `list_objects()` returns `S3Object.key` as full S3 keys (with configured prefix). Use `strip_s3_prefix()` to convert to relative keys before comparing against `referenced_keys` (which uses the same format as `PartInfo.backup_key` — relative keys like `"backup_name/data/db/table/part.tar.lz4"`)
     - Partition keys into: manifest key (`{backup_name}/metadata.json`) and data keys
     - For data keys: filter to those NOT in `referenced_keys` (compare using stripped/relative keys)
     - Log: `info!(total_keys = ..., unreferenced = ..., referenced = ..., "gc: deleting N unreferenced keys, preserving N referenced")`
     - Batch delete unreferenced data keys via `s3.delete_objects(unreferenced_keys).await?` (pass the original keys from `list_objects`, since `delete_objects` expects the same format)
     - Delete the manifest key last via `s3.delete_objects(vec![manifest_key]).await?`
   - `retention_remote(s3: &S3Client, keep: i32) -> Result<usize>` (public):
     - `keep == 0` => return Ok(0) (unlimited)
     - Call `list_remote(s3).await?`
     - Filter out broken backups
     - Sort valid by timestamp ascending
     - If valid count <= keep => return Ok(0)
     - For each backup to delete (oldest first):
       - Call `gc_collect_referenced_keys(s3, &backup_name).await?` — loads ALL surviving manifests fresh each iteration (more conservative than design 8.2 step 3c which only reloads new manifests; this approach is safer because it handles any manifest changes, not just additions)
       - Call `gc_delete_backup(s3, &backup_name, &referenced_keys).await?`
       - Note: design 8.2 step 3c race protection is satisfied because `gc_collect_referenced_keys` is called fresh per-backup-deletion. If a concurrent `create_remote` on another host adds a new backup referencing our keys, the next iteration's `gc_collect_referenced_keys` call will see it. The remaining millisecond window between key collection and deletion is mitigated by the global PID lock (per design 8.2 race condition note).
     - Log: `info!(deleted = ..., total = ..., "retention_remote: deleted N of M remote backups")`
     - Return deleted count

3. **Verify tests pass**

**Files:** `src/list.rs`
**Acceptance:** F003

**Implementation Notes:**
- When stripping S3 keys to relative keys, use `strip_s3_prefix()` (private in list.rs, accessible within module)
- The referenced key set must use the same key format as `PartInfo.backup_key` -- both are relative keys like `"backup_name/data/db/table/disk/part.tar.lz4"`
- Errors on individual backup deletions in retention_remote are warnings (continue to next), not fatal

---

### Task 4: Wire CLI clean command + call retention from CLI

**TDD Steps:**

1. **Implement** in `src/main.rs`:
   - Replace the `Command::Clean { name }` stub with:
     ```rust
     Command::Clean { name } => {
         let ch = ChClient::new(&config.clickhouse)?;
         let data_path = &config.clickhouse.data_path;
         let count = list::clean_shadow(&ch, data_path, name.as_deref()).await?;
         info!(removed = count, "Clean command complete");
     }
     ```
   - This requires `clean_shadow` from Task 6. Since Task 6 is in Group C (independent), the actual wiring can only compile after Task 6 is done. The implementation agent should wire this after Task 6 is complete.

2. **Verify:** `cargo check` passes after all tasks are implemented

**Files:** `src/main.rs`
**Acceptance:** F004

**Implementation Notes:**
- The CLI `clean` command only does shadow cleanup (design 13). Retention is triggered via API or watch mode, not CLI. The CLI has `--delete-local` on upload for the -1 semantics.
- Following the existing `Command::CleanBroken` pattern exactly

---

### Task 5: Replace clean_stub with real API handler

**TDD Steps:**

1. **Implement** in `src/server/routes.rs`:
   - Replace `clean_stub()` with a real `clean()` handler following the operation endpoint pattern:
     ```rust
     pub async fn clean(
         State(state): State<AppState>,
     ) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
         let (id, _token) = state.try_start_op("clean").await.map_err(|e| {
             (StatusCode::CONFLICT, Json(ErrorResponse { error: e.to_string() }))
         })?;
         let state_clone = state.clone();
         tokio::spawn(async move {
             info!("Starting clean operation");
             let start_time = std::time::Instant::now();
             let data_path = state_clone.config.clickhouse.data_path.clone();
             let result = list::clean_shadow(&state_clone.ch, &data_path, None).await;
             let duration = start_time.elapsed().as_secs_f64();
             match result {
                 Ok(count) => {
                     if let Some(m) = &state_clone.metrics {
                         m.backup_duration_seconds.with_label_values(&["clean"]).observe(duration);
                         m.successful_operations_total.with_label_values(&["clean"]).inc();
                     }
                     info!(count = count, "clean operation completed");
                     state_clone.finish_op(id).await;
                 }
                 Err(e) => {
                     if let Some(m) = &state_clone.metrics {
                         m.backup_duration_seconds.with_label_values(&["clean"]).observe(duration);
                         m.errors_total.with_label_values(&["clean"]).inc();
                     }
                     warn!(error = %e, "clean operation failed");
                     state_clone.fail_op(id, e.to_string()).await;
                 }
             }
         });
         Ok(Json(OperationStarted { id, status: "started".to_string() }))
     }
     ```
   - Update `src/server/mod.rs` route wiring: change `post(routes::clean_stub)` to `post(routes::clean)`
   - Remove the `clean_stub` function
   - Update `test_stub_endpoints_return_501` test at routes.rs:1239: remove the 3 lines that call `clean_stub()` and assert on "Phase 3c" (lines 1240-1242). The remaining stub assertions (reload, restart, tables) stay.

2. **Verify:** `cargo check` and `cargo test -p chbackup --lib` pass

**Files:** `src/server/routes.rs`, `src/server/mod.rs`
**Acceptance:** F005

**Implementation Notes:**
- Follows the exact pattern from `clean_remote_broken` handler (the closest existing handler)
- The `/api/v1/clean` endpoint does shadow cleanup (per design 9 / 13), not retention
- Retention via API would be a separate endpoint or part of watch mode (Phase 3d)
- The existing `test_stub_endpoints_return_501` test calls `clean_stub()` directly — must remove that assertion or the test won't compile

---

### Task 6: Shadow directory cleanup (clean_shadow)

**TDD Steps:**

1. **Write failing test:** `test_clean_shadow_removes_chbackup_dirs`
   - Create a temp dir simulating a disk path with `{disk_path}/shadow/` containing:
     - `chbackup_daily_mon_default_trades/` (should be removed)
     - `chbackup_weekly_default_events/` (should be removed)
     - `other_freeze_data/` (should NOT be removed -- not chbackup prefix)
   - Call `clean_shadow_dir(disk_path, None)` (the sync inner helper)
   - Assert 2 directories removed, `other_freeze_data/` still exists

2. **Write failing test:** `test_clean_shadow_with_name_filter`
   - Create same temp dir as above
   - Call `clean_shadow_dir(disk_path, Some("daily_mon"))` (filter by backup name)
   - Assert only `chbackup_daily_mon_default_trades/` is removed
   - Assert `chbackup_weekly_default_events/` is still there

3. **Implement** in `src/list.rs`:
   - `clean_shadow_dir(disk_path: &str, name: Option<&str>) -> Result<usize>` (private sync helper):
     - Build shadow path: `{disk_path}/shadow/`
     - If shadow dir does not exist, return Ok(0)
     - Read entries in shadow dir
     - For each entry that is a directory:
       - If name starts with `"chbackup_"`:
         - If `name` filter is Some: only match entries starting with `"chbackup_{name}_"` (using `sanitize_name` on the filter)
         - If `name` filter is None: match all `chbackup_*`
         - Remove the directory via `std::fs::remove_dir_all()`
         - Log: `info!(freeze_name = ..., disk = ..., "clean_shadow: removed shadow directory")`
         - Increment counter
     - Return counter

   - `clean_shadow(ch: &ChClient, data_path: &str, name: Option<&str>) -> Result<usize>` (public async):
     - Call `ch.get_disks().await?` to get all disk paths
     - Filter out backup-type disks (disk_type == "backup") per design 13
     - For each remaining disk: call `clean_shadow_dir(&disk.path, name)` via `tokio::task::spawn_blocking`
     - Sum up all removed counts
     - Log: `info!(total = ..., "clean_shadow: removed N shadow directories")`
     - Return total

4. **Verify tests pass**

**Files:** `src/list.rs`
**Acceptance:** F006

**Implementation Notes:**
- Uses `ChClient::get_disks()` (verified at client.rs:375)
- `DiskRow.disk_type` field uses `#[serde(rename = "type")]` (verified at client.rs:49-50)
- Design 13 says "excluding backup-type disks" -- filter `disk.disk_type != "backup"`
- The `name` filter uses `sanitize_name()` (from `crate::clickhouse::sanitize_name`) to match the freeze name format `chbackup_{sanitized_name}_...`
- Need to add `use crate::clickhouse::{ChClient, sanitize_name};` to list.rs imports

---

### Task 7: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** `src/server`

**TDD Steps:**

1. **Update `src/server/CLAUDE.md`:**
   - In "Stub Endpoints" section: remove `/api/v1/clean` from the stub list (no longer a stub)
   - In "Key Patterns" or appropriate section: document the `clean` endpoint
   - In "Prometheus Metrics" section: add `clean` as a new operation label value for duration, success, and error counters

2. **Validate:** Required sections present:
   - `Parent Context`
   - `Directory Structure`
   - `Key Patterns`
   - `Parent Rules`

**Files:** `src/server/CLAUDE.md`
**Acceptance:** FDOC

**Notes:**
- `src/list.rs` is a single file (no CLAUDE.md subdirectory) -- documented in root CLAUDE.md "Source Module Map"
- This task runs AFTER all code tasks complete
- Preserve existing patterns, only ADD new ones

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All method references verified: list_local, list_remote, delete_local, delete_remote, get_disks, get_object, from_json_bytes, list_objects, delete_objects, strip_s3_prefix, sanitize_name, clean_broken_local/remote patterns |
| RC-008 | PASS | Task 2 defines collect_keys_from_manifest before Task 3 uses it. Task 6 defines clean_shadow before Tasks 4/5 wire it. Config helpers in Task 1 before use in later tasks. |
| RC-015 | PASS | Data flows verified: list_local returns Vec<BackupSummary>, retention_local consumes it. gc_collect_referenced_keys returns HashSet<String>, gc_delete_backup consumes it. |
| RC-016 | PASS | BackupSummary fields used: name (String), timestamp (Option<DateTime<Utc>>), is_broken (bool) -- all verified in symbols.md |
| RC-018 | PASS | Every task has named test functions with specific inputs and assertions |
| RC-019 | PASS | New functions follow clean_broken_local/remote pattern exactly: list->filter->delete->count |
| RC-021 | PASS | All file locations verified: RetentionConfig at config.rs:378, list functions in list.rs, DiskRow at client.rs:46 |

## Notes

### Phase 4.5 (Interface Skeleton Simulation) -- SKIPPED

**Reason:** This plan creates no new types or structs. All new functions are added to existing modules and use only existing types (BackupSummary, BackupManifest, S3Client, ChClient, Config, HashSet). The risk of import/type errors is minimal since every referenced type is already imported or used in the same file.

### Config Resolution Rule

The effective retention config resolution is:
- `retention.backups_to_keep_local` when non-zero, else `general.backups_to_keep_local`
- `retention.backups_to_keep_remote` when non-zero, else `general.backups_to_keep_remote`

This matches the clickhouse-backup Go tool behavior where `retention:` section overrides `general:` section.

### Semantics of keep values

| Value | Meaning |
|-------|---------|
| 0 | Unlimited (no retention action) |
| -1 | Delete local after upload (handled by upload module, NOT by retention) |
| N > 0 | Keep N newest backups, delete oldest exceeding count |

### GC Design 8.2 Race Protection

The plan implements race protection by calling `gc_collect_referenced_keys()` fresh for each backup deletion in the retention loop. This means if backup B2 references keys from B1 (being deleted), those keys are protected because B2's manifest is loaded and its keys are in the referenced set. If a concurrent `create_remote` adds a new backup referencing B1's keys during the retention loop, the next `gc_collect_referenced_keys()` call will see it.
