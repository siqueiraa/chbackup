# Plan: Per-Disk Backup Directories to Avoid EXDEV Cross-Device Copies

## Goal

Eliminate EXDEV cross-device hardlink fallbacks on multi-NVMe ClickHouse setups by placing backup shadow data on the same filesystem as the source disk, instead of always on the default data_path.

## Architecture Overview

Currently, `collect_parts()` hardlinks ALL parts to `{data_path}/backup/{name}/shadow/...` regardless of which disk the source part lives on. When ClickHouse uses multiple local disks on separate NVMe devices, parts on non-default disks cross filesystem boundaries, triggering EXDEV -> full file copy fallback. This is dramatically slower.

The fix changes the staging directory computation so each disk's parts are hardlinked to `{disk_path}/backup/{name}/shadow/...`, keeping hardlinks on the same filesystem. All consumers of the shadow path (upload, download, restore, delete) must be updated to resolve per-disk paths using `BackupManifest.disks`.

**Key invariant:** For single-disk setups, `disk_path == data_path` for the default disk, so the per-disk directory IS the existing directory. Zero behavior change.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **backup_dir (metadata):** Created by `backup::create()` at `{data_path}/backup/{name}/` -- always on default disk. Contains metadata.json, metadata/, access/, configs/.
- **backup shadow (per-disk):** Created by `collect_parts()` at `{disk_path}/backup/{name}/shadow/...` -- on the SAME filesystem as the source data.
- **collect_parts():** Called inside `tokio::spawn_blocking` from `backup::create()`. Already has `disk_name` and `disk_path` in scope during its per-disk walk loop.
- **BackupManifest.disks:** `HashMap<String, String>` mapping disk_name -> disk_path. Already populated during create, persisted in metadata.json. Available at upload/restore/download/delete time.
- **TableManifest.parts:** Already keyed by disk_name. Upload/restore can resolve disk_path from manifest.disks.
- **S3 disk parts:** Unaffected. No hardlinks -- metadata-only. S3 disk type check (`is_s3_disk()`) already gates the hardlink path.

### What This Plan CANNOT Do
- Cannot change the manifest format (must be backward compatible)
- Cannot change the S3 key format (upload keys are independent of local disk layout)
- Cannot break single-disk setups (default disk path == data_path must produce identical layout)
- Cannot break download -> restore flow (downloaded parts must be findable by restore)
- Cannot require all original disks to exist at restore/download time (graceful fallback required)
- Cannot change how S3 disk parts are handled (they use CopyObject, not hardlinks)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Breaking single-disk setups | GREEN | When disk_path == data_path, per-disk dir IS existing dir. Unit test verifies. |
| Old backups (pre per-disk) not findable | GREEN | `resolve_shadow_part_path()` tries per-disk candidate, then legacy `backup_dir/shadow/` path. Part-existence check, not just disk-existence. |
| Download to host with different disks | GREEN | `resolve_shadow_part_path()` fallback chain handles missing per-disk path gracefully. |
| delete_local missing per-disk dirs | YELLOW | Loads manifest (or download state file) to discover disk paths. Canonicalizes paths before dedup to avoid double-delete via symlinks. |
| Restore remap with per-disk dirs | YELLOW | `OwnedAttachParams` carries `source_db`/`source_table` separately from destination names. Shadow lookup always uses source names. |
| Failed download leaks per-disk data | YELLOW | Download state file includes `disk_map` so `delete_local` can clean per-disk dirs even without manifest. |
| collect_parts signature change breaks tests | GREEN | Only 2 test callers + 1 production caller. Small migration. |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `staging per-disk backup dir` | yes | Logged during collect_parts per-disk staging |
| `Deleting per-disk backup dir` | yes (multi-disk only) | Logged during delete_local for non-default disk cleanup |
| `EXDEV` | no (forbidden on same-disk) | Should NOT appear for parts on per-disk dirs |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| collect_parts has 10 params (clippy::too_many_arguments) | Already suppressed with allow; adding backup_name makes 10. Refactoring to a params struct is a separate concern. | Future refactoring plan |
| Download hardlink dedup only searches data_path backups | The `find_existing_part()` function could also search per-disk backup dirs, but this is an optimization, not a correctness fix. | Phase 9+ |

## Dependency Groups

```
Group A (Sequential -- Core Path: Create + Upload):
  Task 1: Add per_disk_backup_dir() helper
  Task 2: Update collect_parts() to use per-disk staging dirs
  Task 3: Update find_part_dir() for per-disk part lookup
  Task 4: Update upload() delete_local to clean per-disk dirs

Group B (Sequential -- Download Path):
  Task 5: Update download() to write per-disk dirs
  Task 6: Update download find_existing_part() for per-disk search

Group C (Sequential -- Restore Path):
  Task 7: Add manifest disks to OwnedAttachParams + per-disk resolution in attach_parts_inner
  Task 8: Update ATTACH TABLE mode per-disk path in restore/mod.rs

Group D (Sequential -- Delete + Cleanup):
  Task 9: Update delete_local() to clean all per-disk backup dirs
  Task 10: Update backup::create() error cleanup for per-disk dirs

Group E (Final -- Documentation, depends on A-D):
  Task 11: Update CLAUDE.md for all modified modules
```

## Tasks

### Task 1: Add per_disk_backup_dir() and resolve_shadow_part_path() helpers

**TDD Steps:**
1. Write unit test `test_per_disk_backup_dir_default_disk` verifying that when `disk_path == data_path`, the result equals `{data_path}/backup/{name}`
2. Write unit test `test_per_disk_backup_dir_non_default_disk` verifying that when `disk_path != data_path`, the result is `{disk_path}/backup/{name}`
3. Write unit test `test_resolve_shadow_part_path_per_disk_exists` verifying per-disk candidate is returned when it exists
4. Write unit test `test_resolve_shadow_part_path_fallback_to_legacy_encoded` verifying fallback to `backup_dir/shadow/{encoded_db}/{encoded_table}/...` when per-disk path doesn't exist (old backups with manifest.disks populated but legacy layout)
5. Write unit test `test_resolve_shadow_part_path_fallback_to_legacy_plain` verifying fallback to `backup_dir/shadow/{plain_db}/{plain_table}/...` when neither per-disk nor encoded legacy path exists (very old backups without URL encoding)
6. Write unit test `test_resolve_shadow_part_path_no_disk_in_manifest` verifying fallback when disk_name is not in manifest.disks
7. Write unit test `test_resolve_shadow_part_path_plain_skipped_when_same` verifying step 3 is skipped when plain == encoded (no redundant FS check)
8. Implement both helpers in `src/backup/collect.rs`
9. Verify tests pass

**Implementation Notes:**
```rust
/// Compute the per-disk backup directory for a given disk.
pub fn per_disk_backup_dir(disk_path: &str, backup_name: &str) -> PathBuf {
    PathBuf::from(disk_path).join("backup").join(backup_name)
}

/// Resolve the shadow part path with strict fallback order:
/// 1. Per-disk candidate (encoded):  {disk_path}/backup/{name}/shadow/{db}/{table}/{part}/
/// 2. Legacy default (encoded):      {backup_dir}/shadow/{db}/{table}/{part}/
/// 3. Legacy default (plain):        {backup_dir}/shadow/{plain_db}/{plain_table}/{part}/
/// 4. None (part not found at any location)
///
/// `db` and `table` are expected to be URL-encoded (as created by backup::collect).
/// `plain_db` and `plain_table` are the original unencoded names (for very old backups
/// that stored shadow dirs without URL encoding).
///
/// This is the SINGLE source of truth for shadow path resolution across
/// upload, download, and restore. Consumers must not implement their own
/// fallback logic.
pub fn resolve_shadow_part_path(
    backup_dir: &Path,
    manifest_disks: &HashMap<String, String>,
    backup_name: &str,
    disk_name: &str,
    encoded_db: &str,
    encoded_table: &str,
    plain_db: &str,
    plain_table: &str,
    part_name: &str,
) -> Option<PathBuf> {
    let encoded_suffix = PathBuf::from("shadow")
        .join(encoded_db).join(encoded_table).join(part_name);

    // 1. Try per-disk candidate (encoded path)
    if let Some(disk_path) = manifest_disks.get(disk_name) {
        let per_disk = per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
        let candidate = per_disk.join(&encoded_suffix);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 2. Fallback to legacy encoded path (covers old backups and single-disk setups)
    let legacy_encoded = backup_dir.join(&encoded_suffix);
    if legacy_encoded.exists() {
        return Some(legacy_encoded);
    }

    // 3. Fallback to legacy plain path (very old backups without URL encoding)
    if plain_db != encoded_db || plain_table != encoded_table {
        let plain_suffix = PathBuf::from("shadow")
            .join(plain_db).join(plain_table).join(part_name);
        let legacy_plain = backup_dir.join(&plain_suffix);
        if legacy_plain.exists() {
            return Some(legacy_plain);
        }
    }

    // 4. Not found
    None
}
```

**Key design decisions:**
- Fallback checks part-path existence, NOT disk-path existence. Correctly handles old backups with `manifest.disks` populated but legacy single-dir layout.
- Preserves the existing plain-path fallback from `find_part_dir()` (upload/mod.rs line 1080-1088) that handles very old backups without URL-encoded shadow dirs.
- Step 3 is skipped when plain == encoded (common case) to avoid redundant filesystem checks.

**Files:** `src/backup/collect.rs`
**Acceptance:** F001

---

### Task 2: Update collect_parts() to use per-disk staging directories

**TDD Steps:**
1. Write unit test `test_collect_parts_per_disk_staging_dir` using a tempdir with two "disks" (two subdirs on same FS). Verify parts from each "disk" are hardlinked to their respective `{disk_path}/backup/{name}/shadow/` directory, not the default backup_dir.
2. Add `backup_name: &str` parameter to `collect_parts()` (position: after `backup_dir`)
3. Change staging_dir computation (currently line ~303) from:
   ```rust
   let staging_dir = backup_dir.join("shadow")...
   ```
   to:
   ```rust
   let per_disk_dir = per_disk_backup_dir(disk_path, backup_name);
   info!(disk = %disk_name, path = %per_disk_dir.display(), "staging per-disk backup dir");
   let staging_dir = per_disk_dir.join("shadow")...
   ```
4. Update the caller in `backup/mod.rs` (line ~507) to pass `backup_name`
5. Update the 2 test callers in `backup/collect.rs` to pass a `backup_name` string
6. Verify all tests pass, `cargo check` clean

**Note:** The `info!` log at step 3 satisfies the required runtime log pattern `staging per-disk backup dir` from the Expected Runtime Logs table.

**Files:** `src/backup/collect.rs`, `src/backup/mod.rs`
**Acceptance:** F002

---

### Task 3: Update find_part_dir() to use resolve_shadow_part_path()

**TDD Steps:**
1. Write unit test `test_find_part_dir_per_disk` that creates a tempdir simulating per-disk layout and verifies `find_part_dir` finds the part at the per-disk path
2. Write unit test `test_find_part_dir_fallback_default` that creates a tempdir simulating old (single-dir) layout and verifies `find_part_dir` still finds it (backward compat)
3. Write unit test `test_find_part_dir_old_backup_with_manifest_disks` that creates a tempdir where `manifest.disks` has an entry but data is in legacy `backup_dir/shadow/` layout — verifies fallback works for old backups
4. Change `find_part_dir()` to delegate to `resolve_shadow_part_path()` from `backup::collect`. Pass `manifest_disks`, `backup_name`, and `disk_name` through.
5. Update callers in `upload()` (lines ~361 and ~379) to pass `&manifest.disks`, `backup_name`, and `disk_name`. All are already in scope.
6. Verify tests pass, backward compat test passes

**Implementation Notes:**
Updated signature:
```rust
fn find_part_dir(
    backup_dir: &Path,
    db: &str,
    table: &str,
    part_name: &str,
    manifest_disks: &HashMap<String, String>,
    backup_name: &str,
    disk_name: &str,
) -> Result<PathBuf>
```

Body delegates to `resolve_shadow_part_path()`:
```rust
use crate::backup::collect::resolve_shadow_part_path;

fn find_part_dir(...) -> Result<PathBuf> {
    let url_db = url_encode_component(db);
    let url_table = url_encode_component(table);
    resolve_shadow_part_path(
        backup_dir, manifest_disks, backup_name,
        disk_name, &url_db, &url_table, db, table, part_name,
    ).ok_or_else(|| anyhow::anyhow!(
        "Part directory not found for {db}.{table}/{part_name} (checked per-disk, legacy encoded, and legacy plain paths)"
    ))
}
```

**Files:** `src/upload/mod.rs`
**Acceptance:** F003

---

### Task 4: Update upload() delete_local to clean per-disk dirs

**TDD Steps:**
1. Write unit test `test_upload_delete_local_cleans_per_disk_dirs` verifying that when delete_local is triggered, per-disk backup dirs are also removed
2. Replace the existing `remove_dir_all(backup_dir)` (line ~989) with per-disk cleanup first (non-fatal), then default backup_dir last (fatal). See implementation notes below.
3. Verify test passes

**Implementation Notes:**
The `manifest` is in scope (loaded at line ~200). The `data_path` is available from `config.clickhouse.data_path`. Per-disk dirs are deleted first (non-fatal warn on failure), default backup_dir is deleted last (fatal, preserving existing `?` propagation semantics).

**Path dedup via canonicalize:** Use `std::fs::canonicalize()` to resolve symlinks before comparing. Collect unique paths into a `HashSet` to prevent double-delete. If `canonicalize()` fails (path doesn't exist yet), fall back to the raw path.

```rust
use std::collections::HashSet;

if delete_local {
    // First: delete per-disk dirs (non-fatal, warn on failure)
    // These are "bonus" cleanup -- failing here is not critical.
    let canonical_default = std::fs::canonicalize(&backup_dir)
        .unwrap_or_else(|_| backup_dir.clone());
    let mut seen: HashSet<PathBuf> = HashSet::new();
    seen.insert(canonical_default);

    for (_, disk_path) in &manifest.disks {
        let per_disk = per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
        if per_disk.exists() {
            let canonical = std::fs::canonicalize(&per_disk)
                .unwrap_or_else(|_| per_disk.clone());
            if seen.insert(canonical) {
                if let Err(e) = std::fs::remove_dir_all(&per_disk) {
                    warn!(path = %per_disk.display(), error = %e,
                        "Failed to remove per-disk backup dir");
                }
            }
        }
    }

    // Last: delete default backup_dir (FATAL with ? -- preserves existing semantics)
    info!(backup_dir = %backup_dir.display(), "Deleting local backup after upload");
    std::fs::remove_dir_all(&backup_dir).with_context(|| {
        format!("Failed to delete local backup directory: {}", backup_dir.display())
    })?;
}
```

**Note:** Default backup_dir deletion remains fatal (propagates via `?`) to preserve existing semantics from `mod.rs` line 989. Only per-disk dir deletion is non-fatal (warn on failure), since those are supplementary cleanup.

**Files:** `src/upload/mod.rs`
**Acceptance:** F004

---

### Task 5: Update download() to write parts to per-disk directories + persist disk map in state

**TDD Steps:**
1. Write unit test `test_download_per_disk_dir_construction` verifying that when `manifest.disks` maps a disk to a non-default path AND the disk path exists locally, the shadow_dir uses `{disk_path}/backup/{name}/shadow/...`
2. Write unit test `test_download_per_disk_fallback_disk_not_present` verifying fallback to `{data_path}/backup/{name}/shadow/...` when the disk path doesn't exist on the host
3. In the download parallel task (lines ~468 for S3 disk parts, line ~548 for local parts):
   - Check if `disk_path` from `manifest.disks` exists on local host
   - If yes: write to `{disk_path}/backup/{name}/shadow/...` (per-disk)
   - If no: write to default `{data_path}/backup/{name}/shadow/...` (fallback for cross-host download)
4. **Persist disk map unconditionally (NOT gated by resume mode):** Add `disk_map: HashMap<String, String>` field to `DownloadState` in `resume.rs` (with `#[serde(default)]` for backward compat). Write a minimal state file with just the disk map BEFORE downloading any parts, regardless of whether `use_resume` is true. This is a separate `save_state_graceful()` call inserted AFTER `backup_dir` creation (line ~300) and BEFORE the download work queue (line ~340). This ensures `delete_local` can discover per-disk dirs even if the download fails before writing `metadata.json`, whether or not resume mode is active.
5. Write unit test `test_download_disk_map_persisted_without_resume` verifying that disk_map is written to the state file even when `resume=false`
6. Verify tests pass

**Implementation Notes:**
The `item.disk_name` is already on `DownloadWorkItem`. Clone `manifest.disks` (or Arc) into spawned tasks.

For the target dir, we check disk-path existence (not part-path existence) because during download we are CREATING the paths, not finding existing ones:
```rust
// Resolve per-disk target dir for this part's disk (download creates these paths)
let target_backup_dir = match manifest_disks.get(&item.disk_name) {
    Some(dp) if Path::new(dp.trim_end_matches('/')).exists() => {
        per_disk_backup_dir(dp.trim_end_matches('/'), backup_name)
    }
    _ => backup_dir.clone(), // Disk not on this host -- fall back to default
};
let shadow_dir = target_backup_dir.join("shadow").join(&url_db).join(&url_table).join(&item.part.name);
```

**Note on resolve_shadow_part_path() vs direct construction:** During download we are WRITING (creating dirs), not reading. `resolve_shadow_part_path()` is for READ paths (checking existence). Download uses direct `per_disk_backup_dir()` + disk-existence check for WRITE paths.

**Disk map persistence (unconditional, NOT gated by use_resume):**
```rust
// After backup_dir creation (line ~300), BEFORE work queue construction:
// Always persist disk map so delete_local can find per-disk dirs on failure.
// This is independent of resume mode -- it's for cleanup, not resume.
if !manifest.disks.is_empty() {
    let disk_map_state = DownloadState {
        completed_keys: HashSet::new(),
        backup_name: backup_name.to_string(),
        params_hash: current_params_hash.clone(),
        disk_map: manifest.disks.clone(),
    };
    save_state_graceful(&state_path, &disk_map_state);
}
```

**DownloadState extension:**
```rust
// In resume.rs
pub struct DownloadState {
    // ... existing fields (completed_keys, backup_name, params_hash) ...
    /// Disk name -> disk path mapping from manifest. Persisted so delete_local
    /// can discover per-disk dirs even if download fails before writing metadata.json.
    /// Written unconditionally (not gated by resume mode) for cleanup safety.
    #[serde(default)]
    pub disk_map: HashMap<String, String>,
}
```

**Files:** `src/download/mod.rs`, `src/resume.rs`
**Acceptance:** F005

---

### Task 6: Update download find_existing_part() for per-disk search

**TDD Steps:**
1. Write unit test `test_find_existing_part_per_disk` creating a tempdir simulating per-disk layout in an "existing" backup, verifying that `find_existing_part` can find the part at a per-disk path
2. Add `manifest_disks: &HashMap<String, String>` and `disk_name: &str` parameters to `find_existing_part()`
3. When searching existing backups, also check per-disk paths: `{disk_path}/backup/{other_backup}/shadow/{table_key}/{part_name}/`
4. Update caller in download() to pass manifest.disks and disk_name from the work item
5. Verify test passes

**Implementation Notes:**
The current function (lines 132-192) searches `{data_path}/backup/*/shadow/...`. We add an additional search: for each disk in `manifest_disks` where disk_path != data_path, also check `{disk_path}/backup/*/shadow/...`.

```rust
fn find_existing_part(
    data_path: &str,
    current_backup: &str,
    table_key: &str,
    part_name: &str,
    expected_crc: u64,
    manifest_disks: &HashMap<String, String>,
    disk_name: &str,
) -> Option<PathBuf>
```

**Files:** `src/download/mod.rs`
**Acceptance:** F006

---

### Task 7: Add manifest_disks + source_db/source_table to OwnedAttachParams, use resolve_shadow_part_path()

**TDD Steps:**
1. Write unit test `test_attach_source_dir_per_disk` verifying that when `manifest_disks` maps a part's disk to a non-default path, the source resolves to per-disk path
2. Write unit test `test_attach_source_dir_remap_uses_source_names` verifying that when `source_db != db` (remap active), the shadow lookup uses `source_db`/`source_table`, NOT destination names
3. Write unit test `test_attach_source_dir_old_backup_fallback` verifying that old backups with manifest.disks populated but legacy layout fall through to legacy path
4. Add fields to `OwnedAttachParams`:
   - `manifest_disks: HashMap<String, String>` — disk name → disk path from manifest
   - `source_db: String` — original (pre-remap) database name for shadow path lookup
   - `source_table: String` — original (pre-remap) table name for shadow path lookup
5. In `attach_parts_inner()` (line ~515), change source_dir computation to use `resolve_shadow_part_path()` with `source_db`/`source_table`:
   - Build reverse map `part_name -> disk_name` from `parts_by_disk`
   - Use `resolve_shadow_part_path(backup_dir, manifest_disks, backup_name, disk_name, url_encode(source_db), url_encode(source_table), part_name)`
6. Similarly update `restore_s3_disk_parts()` (line ~291) to use `source_db`/`source_table` for shadow path and `resolve_shadow_part_path()` for per-disk resolution
7. Update the caller in `restore/mod.rs` (line ~537) to populate all new fields:
   - `manifest_disks: manifest.disks.clone()`
   - `source_db: src_db.to_string()` (already available at line ~472)
   - `source_table: src_table.to_string()`
8. Verify tests pass

**Implementation Notes:**

**Why source_db/source_table:** The shadow directory structure is created during `backup::create()` using the original ClickHouse table names. When remap is active (`--as` or `-m`), `params.db`/`params.table` are the DESTINATION names, but the shadow path uses SOURCE names. Currently `attach_parts_inner()` at line 515 uses `url_encode(db)` (destination) — this is a pre-existing bug that only manifests with remap. `try_attach_table_mode()` at line 985 correctly uses `src_db`/`src_table`. This task fixes the inconsistency.

```rust
// In OwnedAttachParams:
pub source_db: String,
pub source_table: String,
pub manifest_disks: HashMap<String, String>,
```

In `attach_parts_inner()`:
```rust
use crate::backup::collect::resolve_shadow_part_path;

let url_src_db = url_encode(&params.source_db);
let url_src_table = url_encode(&params.source_table);
let backup_name = params.backup_dir.file_name()
    .and_then(|n| n.to_str()).unwrap_or("unknown");

let part_to_disk: HashMap<String, String> = params.parts_by_disk.iter()
    .flat_map(|(disk, parts)| parts.iter().map(move |p| (p.name.clone(), disk.clone())))
    .collect();

// ... in the per-part loop:
let disk_name = part_to_disk.get(&part.name).map(String::as_str).unwrap_or("default");
let source_dir = resolve_shadow_part_path(
    &params.backup_dir, &params.manifest_disks, backup_name,
    disk_name, &url_src_db, &url_src_table,
    &params.source_db, &params.source_table, &part.name,
);
match source_dir {
    Some(dir) => { /* hardlink from dir */ }
    None => {
        warn!(part = %part.name, "Part source directory not found, skipping");
        continue;
    }
}
```

**Files:** `src/restore/attach.rs`, `src/restore/mod.rs`
**Acceptance:** F007

---

### Task 8: Update ATTACH TABLE mode to use resolve_shadow_part_path() with source names

**TDD Steps:**
1. Write unit test `test_attach_table_mode_per_disk_shadow` verifying that the per-part source path in ATTACH TABLE mode resolves via `resolve_shadow_part_path()` to the per-disk location
2. Write unit test `test_attach_table_mode_remap_uses_source_names` verifying ATTACH TABLE mode uses `src_db`/`src_table` (which it already does at line 985), NOT destination names
3. In `try_attach_table_mode()` (line ~987 of restore/mod.rs):
   - Add `manifest_disks: &HashMap<String, String>` and `parts_by_disk: &HashMap<String, Vec<PartInfo>>` parameters
   - Build `part_to_disk` reverse map
   - Replace single `shadow_base` with per-part `resolve_shadow_part_path()` call inside the loop
   - Note: this function already correctly uses `src_db`/`src_table` for shadow lookup (line 985)
4. Thread `manifest_disks` and `parts_by_disk` from the caller (which already has them from `OwnedAttachParams`)
5. Verify test passes

**Implementation Notes:**
`try_attach_table_mode()` already correctly uses `src_db`/`src_table` at line 985. The change is moving from a single `shadow_base` to per-part resolution.

Inside the `spawn_blocking` block (line ~992):
```rust
use crate::backup::collect::resolve_shadow_part_path;

let part_to_disk: HashMap<String, String> = parts_by_disk.iter()
    .flat_map(|(disk, parts)| parts.iter().map(move |p| (p.name.clone(), disk.clone())))
    .collect();

let url_db = attach::url_encode(src_db);
let url_table = attach::url_encode(src_table);
let backup_name = backup_dir.file_name()
    .and_then(|n| n.to_str()).unwrap_or("unknown");

for part_name in &parts_owned {
    let disk_name = part_to_disk.get(part_name).map(String::as_str).unwrap_or("default");
    let part_src = match resolve_shadow_part_path(
        &backup_dir_clone, &manifest_disks, backup_name,
        disk_name, &url_db, &url_table, src_db, src_table, part_name,
    ) {
        Some(p) => p,
        None => continue, // Part not found at any location
    };
    let part_dst = data_path.join(part_name);
    // ... rest unchanged
}
```

**Files:** `src/restore/mod.rs`
**Acceptance:** F008

---

### Task 9: Update delete_local() with manifest + state file fallback + path canonicalization

**TDD Steps:**
1. Write unit test `test_delete_local_cleans_per_disk_dirs` creating a tempdir with metadata.json containing disks map and per-disk backup dirs, verifying all are removed
2. Write unit test `test_delete_local_no_manifest_uses_download_state` verifying that when metadata.json is missing but `download.state.json` has `disk_map`, per-disk dirs are still cleaned
3. Write unit test `test_delete_local_no_manifest_no_state_fallback` verifying that when neither manifest nor state file exists (broken backup), only the default dir is removed (existing behavior)
4. Write unit test `test_delete_local_symlink_dedup` verifying that when two disk paths resolve to the same canonical path, the directory is only deleted once
5. Modify `delete_local()` in `src/list.rs`:
   - Try to load disk map from manifest first, then fall back to download state file
   - Collect all per-disk dirs, canonicalize, dedupe via HashSet
   - Always delete the default backup_dir last
6. Verify tests pass

**Implementation Notes:**
```rust
use std::collections::HashSet;
use crate::backup::collect::per_disk_backup_dir;

pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()> {
    let backup_dir = PathBuf::from(data_path).join("backup").join(backup_name);

    if !backup_dir.exists() {
        return Err(anyhow::anyhow!(...));
    }

    // Discover disk map: manifest first, download state file as fallback
    let disk_map: HashMap<String, String> = {
        let manifest_path = backup_dir.join("metadata.json");
        match BackupManifest::load_from_file(&manifest_path) {
            Ok(m) => m.disks,
            Err(_) => {
                // Fallback: try download state file (persisted unconditionally during download)
                let state_path = backup_dir.join("download.state.json");
                match load_state_file::<DownloadState>(&state_path) {
                    Ok(Some(s)) => s.disk_map,
                    _ => HashMap::new(), // No manifest, no state -- only default dir
                }
            }
        }
    };

    info!(backup = %backup_name, path = %backup_dir.display(), "Deleting local backup");

    // Collect all dirs to delete, deduped by canonical path
    let mut dirs_to_delete: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    // Default backup_dir always included (deleted last)
    let canonical_default = std::fs::canonicalize(&backup_dir)
        .unwrap_or_else(|_| backup_dir.clone());
    seen.insert(canonical_default);

    // Per-disk dirs (skip if same canonical path as default)
    for (_, disk_path) in &disk_map {
        let per_disk = per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
        if per_disk.exists() {
            let canonical = std::fs::canonicalize(&per_disk)
                .unwrap_or_else(|_| per_disk.clone());
            if seen.insert(canonical) {
                dirs_to_delete.push(per_disk);
            }
        }
    }

    // Delete per-disk dirs first (non-fatal)
    for dir in &dirs_to_delete {
        info!(path = %dir.display(), "Deleting per-disk backup dir");
        if let Err(e) = std::fs::remove_dir_all(dir) {
            warn!(path = %dir.display(), error = %e, "Failed to remove per-disk backup dir");
        }
    }

    // Delete default backup_dir last (fatal on failure)
    std::fs::remove_dir_all(&backup_dir)?;
    info!(backup = %backup_name, "Local backup deleted");
    Ok(())
}
```

**Files:** `src/list.rs`, `src/resume.rs` (import DownloadState)
**Acceptance:** F009

---

### Task 10: Update backup::create() error cleanup for per-disk dirs

**TDD Steps:**
1. Write unit test `test_create_error_cleanup_per_disk` verifying that when backup create fails (e.g., mock FREEZE failure), per-disk backup dirs created so far are cleaned up
2. In `backup/mod.rs` error cleanup (line ~594), after removing the default backup_dir, also remove per-disk dirs using the `disk_map` that is in scope
3. Verify test passes

**Implementation Notes:**
The `disk_map` (`HashMap<String, String>` of disk_name -> disk_path) is already in scope at the error cleanup site (it was built at lines ~136-139 from `ch.get_disks()`).

Uses same canonicalize + HashSet dedup pattern as Tasks 4 and 9:

```rust
use std::collections::HashSet;

if let Some(e) = first_error {
    // Collect all dirs to clean, deduped by canonical path
    let mut dirs_to_delete: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    // Default backup_dir
    if backup_dir.exists() {
        let canonical = std::fs::canonicalize(&backup_dir)
            .unwrap_or_else(|_| backup_dir.clone());
        seen.insert(canonical);
        dirs_to_delete.push(backup_dir.clone());
    }

    // Per-disk dirs
    for (_, disk_path) in &disk_map {
        let per_disk = per_disk_backup_dir(disk_path.trim_end_matches('/'), backup_name);
        if per_disk.exists() {
            let canonical = std::fs::canonicalize(&per_disk)
                .unwrap_or_else(|_| per_disk.clone());
            if seen.insert(canonical) {
                dirs_to_delete.push(per_disk);
            }
        }
    }

    for dir in &dirs_to_delete {
        if let Err(rm_err) = std::fs::remove_dir_all(dir) {
            warn!(path = %dir.display(), error = %rm_err,
                "Failed to clean up backup dir after error");
        } else {
            info!(path = %dir.display(), "Removed backup dir after error");
        }
    }

    // Clean shadow directories
    match crate::list::clean_shadow(ch, &config.clickhouse.data_path, Some(backup_name)).await { ... }
}
```

**Files:** `src/backup/mod.rs`
**Acceptance:** F010

---

### Task 11: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects per-disk backup directory changes.

**Modules to update:** src/backup, src/upload, src/download, src/restore

**TDD Steps:**

1. **Read affected-modules.json for module list:**
   Per context/affected-modules.json: src/backup, src/upload, src/download, src/restore

2. **For each module, regenerate directory tree:**
   Use `tree -L 2 {module} --noreport 2>/dev/null || ls -la {module}` and update the Directory Structure section.

3. **Detect and add new patterns:**
   - src/backup/CLAUDE.md: Add "Per-Disk Backup Directory" pattern (per_disk_backup_dir helper, collect_parts backup_name param, staging_dir computation change)
   - src/upload/CLAUDE.md: Add per-disk find_part_dir resolution, per-disk delete_local cleanup
   - src/download/CLAUDE.md: Add per-disk download target dir, per-disk find_existing_part search
   - src/restore/CLAUDE.md: Add manifest_disks on OwnedAttachParams, per-disk source_dir resolution, per-disk ATTACH TABLE mode

4. **Validate all CLAUDE.md files:**
   Check required sections: Parent Context, Directory Structure, Key Patterns, Parent Rules

5. **Update root CLAUDE.md:**
   Add "Per-disk backup directories" to Key Implementation Patterns section:
   - `per_disk_backup_dir(disk_path, backup_name) -> PathBuf` helper in collect.rs
   - collect_parts stages to `{disk_path}/backup/{name}/shadow/...` instead of single backup_dir
   - All consumers (upload/download/restore/delete) resolve per-disk paths via manifest.disks
   - Backward compat: single-disk (disk_path == data_path) produces identical layout

**Files:** `src/backup/CLAUDE.md`, `src/upload/CLAUDE.md`, `src/download/CLAUDE.md`, `src/restore/CLAUDE.md`, `CLAUDE.md`
**Acceptance:** FDOC

---

## Notes

### Phase 4.5: Interface Skeleton Simulation

**Skip reason:** All changes are within existing functions (modifying signatures of private/pub functions and adding parameters). No new public structs, traits, or modules are introduced. The only new symbol is `per_disk_backup_dir()` which is a trivial `PathBuf` construction that does not require compilation verification. The existing `cargo check` clean baseline (0 errors, 0 warnings) is the baseline; each task will verify `cargo check` passes after changes.

### Backward Compatibility Design

This plan carefully maintains backward compatibility through these mechanisms:

1. **resolve_shadow_part_path() with part-existence fallback:** Single helper used by upload, restore, and ATTACH TABLE mode. Tries per-disk candidate first, then falls back to legacy `backup_dir/shadow/` path. Checks actual **part path existence** (not just disk-path existence), so old backups with `manifest.disks` populated but legacy layout are handled correctly.

2. **delete_local multi-source discovery:** Loads disk map from manifest first, then falls back to download state file (`disk_map` field persisted early during download). Only if both are missing (broken backup) does it fall back to cleaning just the default dir.

3. **Path canonicalization for cleanup:** All delete paths (Tasks 4, 9, 10) canonicalize paths and dedupe via `HashSet` before deletion, preventing double-delete when symlinks or equivalent paths resolve to the same directory.

4. **Download disk existence check:** Before writing to per-disk dir, checks if the disk path exists on the local host. If not (cross-host download), falls back to data_path. Note: download WRITES (creates dirs), so it checks disk existence, not part existence.

5. **Remap-safe source paths:** `OwnedAttachParams` carries `source_db`/`source_table` separately from destination `db`/`table`. Shadow lookup always uses source names, fixing a pre-existing inconsistency between `attach_parts_inner()` (used destination names) and `try_attach_table_mode()` (correctly used source names).

6. **Single-disk identity:** When there's only one disk (common case), `disk_path == data_path`, so per-disk dir IS the existing backup dir. No behavior change whatsoever.

7. **DownloadState backward compat:** New `disk_map` field uses `#[serde(default)]` so existing state files without it deserialize cleanly to empty HashMap.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All functions verified via grep: collect_parts, find_part_dir, delete_local, hardlink_dir, per_disk_backup_dir (new), is_s3_disk |
| RC-008 | PASS | Task 1 defines per_disk_backup_dir before Task 2-10 use it. Task 2 adds backup_name param before Task 3+ depend on it. |
| RC-015 | PASS | per_disk_backup_dir returns PathBuf, consumed as PathBuf by all callers |
| RC-016 | PASS | No new structs. OwnedAttachParams gets one new field (manifest_disks: HashMap<String, String>) |
| RC-017 | PASS | manifest_disks field added in Task 7, used in Tasks 7+8. OwnedAttachParams is existing struct. |
| RC-018 | PASS | Every task has named test functions with specific assertions |
| RC-019 | PASS | per_disk_backup_dir follows PathBuf::from().join().join() pattern used throughout codebase |
| RC-021 | PASS | All file locations verified: collect_parts at collect.rs:116, find_part_dir at upload/mod.rs:1065, etc. |
| RC-035 | PASS | cargo fmt step included in execution (zero warnings policy) |
