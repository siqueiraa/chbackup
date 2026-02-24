# Affected Modules Analysis

## Summary

- **Modules to update:** 4
- **Modules to create:** 0
- **Top-level files modified:** 1 (src/list.rs)
- **Git base:** afa01dab

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action | Files |
|--------|------------------|----------|--------|-------|
| src/backup | EXISTS | new_patterns | UPDATE | collect.rs, mod.rs |
| src/upload | EXISTS | new_patterns | UPDATE | mod.rs |
| src/download | EXISTS | new_patterns | UPDATE | mod.rs |
| src/restore | EXISTS | new_patterns | UPDATE | attach.rs, mod.rs |

## Top-Level Files Modified

| File | Purpose |
|------|---------|
| src/list.rs | delete_local() must clean per-disk backup dirs |

## CLAUDE.md Tasks to Generate

1. **Update:** src/backup/CLAUDE.md (per-disk staging pattern, collect_parts signature change)
2. **Update:** src/upload/CLAUDE.md (find_part_dir per-disk resolution)
3. **Update:** src/download/CLAUDE.md (per-disk download target)
4. **Update:** src/restore/CLAUDE.md (per-disk source path resolution)

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **backup_dir**: Created by `backup::create()` at `{data_path}/backup/{name}/`, consumed by upload, download, restore, delete
- **collect_parts()**: Called inside `tokio::spawn` via `spawn_blocking` from `backup::create()`. Receives `backup_dir` as parameter -- this is the key place to change for per-disk staging
- **find_part_dir()**: Called by upload pipeline to locate part data. Must be updated to check per-disk dirs
- **delete_local()**: Called by list::delete, watch cleanup, and upload's delete_local flag. Must remove all per-disk dirs
- **BackupManifest.disks**: Populated by `backup::create()` from `ch.get_disks()`. Already persisted in metadata.json. Available at upload/restore/delete time.

### What This Plan CANNOT Do
- Cannot change the manifest format (must be backward compatible)
- Cannot change the S3 key format (upload keys are independent of local disk layout)
- Cannot break single-disk setups (default disk path == data_path, layout must be unchanged)
- Cannot break download -> restore flow (downloaded parts must be findable by restore)
- Cannot require all original disks to exist at restore time (restore on different host is valid)

### Key Design Decision: Backward Compatibility
- Single-disk setups: `disk_path == data_path` for "default" disk, so per-disk dir == existing dir. NO behavior change.
- Multi-disk setups: Non-default disks get their own `{disk_path}/backup/{name}/shadow/` directories. Default disk continues to use `{data_path}/backup/{name}/shadow/`.
- Old backups (without per-disk dirs): `find_part_dir` falls back to checking `{data_path}/backup/{name}/shadow/` (existing path). Fully backward compatible.
- Manifest format: NO changes. `manifest.disks` already contains disk-path info; `parts` already keyed by disk name.

### Critical Insight: collect_parts Already Knows the Disk
The `collect_parts()` function already has `disk_name` and `disk_path` in scope when hardlinking:
```rust
for (disk_name, disk_path) in &paths_to_walk {
    // ... walk shadow ...
    // HERE: we know which disk this part is on
    let staging_dir = backup_dir.join("shadow")...  // <-- change this to use disk_path
}
```
The change is surgical: replace `backup_dir` with `PathBuf::from(disk_path).join("backup").join(backup_name)` when computing the staging directory.

### Consumer Impact Chain
1. `backup::create()` -> `collect_parts()` writes to per-disk dirs -> metadata.json still at `{data_path}/backup/{name}/`
2. `upload::upload()` -> `find_part_dir()` reads from per-disk dirs (using manifest.disks to resolve)
3. `download::download()` -> writes to per-disk dirs (using manifest.disks to create dirs)
4. `restore::restore()` -> reads from per-disk dirs (using manifest.disks to resolve)
5. `list::delete_local()` -> removes `{data_path}/backup/{name}/` AND per-disk dirs
6. `watch::delete_local_after_upload` -> calls `list::delete_local()` (inherits fix)
7. `upload::delete_local` -> calls `remove_dir_all(backup_dir)` (only removes default; needs manifest.disks too)
