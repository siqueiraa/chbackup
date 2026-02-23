# Symbol and Reference Analysis

## Phase 1: MCP/LSP Symbol Analysis

### 1. `collect_parts()` -- /Users/rafael.siqueira/dev/personal/chbackup/src/backup/collect.rs:116

**Signature (verified):**
```rust
pub fn collect_parts(
    data_path: &str,
    freeze_name: &str,
    backup_dir: &Path,          // <-- hardlink destination (CHANGE TARGET)
    tables: &[TableRow],
    disk_type_map: &HashMap<String, String>,
    disk_paths: &HashMap<String, String>,
    skip_disks: &[String],
    skip_disk_types: &[String],
    skip_projections: &[String],
) -> Result<HashMap<String, Vec<CollectedPart>>>
```

**Callers (via LSP incomingCalls):**
1. `backup::create()` in `src/backup/mod.rs:507` (inside `tokio::task::spawn_blocking`)
2. `test_collect_parts_local_disk_unchanged()` in `src/backup/collect.rs:663`
3. `test_collect_parts_detects_s3_metadata()` in `src/backup/collect.rs:742`

**Critical staging_dir computation (lines 303-307):**
```rust
let staging_dir = backup_dir
    .join("shadow")
    .join(url_encode_path(&db))
    .join(url_encode_path(&table))
    .join(&part_name);
```
This is where the per-disk change must happen. Currently `backup_dir` is always `{data_path}/backup/{name}`.

**Proposed change:** Replace `backup_dir` with per-disk backup dir:
```rust
let per_disk_dir = PathBuf::from(disk_path).join("backup").join(backup_name);
let staging_dir = per_disk_dir
    .join("shadow")
    .join(url_encode_path(&db))
    .join(url_encode_path(&table))
    .join(&part_name);
```

This requires adding `backup_name: &str` to the function signature (the 10th parameter).

### 2. `hardlink_dir()` -- /Users/rafael.siqueira/dev/personal/chbackup/src/backup/collect.rs:403

**Signature (verified):**
```rust
fn hardlink_dir(src_dir: &Path, dst_dir: &Path, skip_proj_patterns: &[String]) -> Result<()>
```

**Callers (via LSP incomingCalls):**
1. `collect_parts()` at line 309 (the key call site)
2. 4 test functions

**No change needed to this function's signature.** The change is in what `dst_dir` value is passed (computed upstream in `collect_parts()`).

### 3. `find_part_dir()` -- /Users/rafael.siqueira/dev/personal/chbackup/src/upload/mod.rs:1065

**Signature (verified):**
```rust
fn find_part_dir(backup_dir: &Path, db: &str, table: &str, part_name: &str) -> Result<PathBuf>
```

**Callers (via LSP incomingCalls):**
1. `upload()` at lines 361 and 379 (local and S3 disk work item construction)
2. `test_find_part_dir_url_encoded()` at line 1253

**Current lookup logic:**
```rust
// Try URL-encoded path first
let path = backup_dir.join("shadow").join(&url_db).join(&url_table).join(part_name);
if path.exists() { return Ok(path); }
// Try plain path as fallback
let plain_path = backup_dir.join("shadow").join(db).join(table).join(part_name);
if plain_path.exists() { return Ok(plain_path); }
// Return URL-encoded path (caller checks existence)
Ok(path)
```

**Required change:** Must search per-disk backup dirs using `manifest.disks` to determine which disk a part is on. The `disk_name` is available in the upload loop (from `table_manifest.parts` which is keyed by disk_name). Two approaches:
- A) Pass `disk_path` alongside `backup_dir` to `find_part_dir` (cleaner)
- B) Try multiple per-disk dirs in sequence (more resilient, backward compatible)

### 4. `delete_local()` -- /Users/rafael.siqueira/dev/personal/chbackup/src/list.rs:477

**Signature (verified):**
```rust
pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()>
```

**Callers (via LSP incomingCalls):**
1. `list::delete()` at line 469
2. `list::clean_broken_local()` at line 563
3. `list::retention_local()` at line 691
4. `server::routes::delete_backup()` at line 994
5. `watch::run_watch_loop()` at line 477
6. 2 test functions

**Current logic:** Only removes `{data_path}/backup/{backup_name}/`
**Required change:** Must also remove per-disk backup dirs. Two options:
- A) Read manifest to find disk paths, then remove `{disk_path}/backup/{backup_name}/` for each non-default disk
- B) Accept a `disks` map parameter (breaks all 5+ callers)

**Option A is preferred:** Load manifest from `{data_path}/backup/{backup_name}/metadata.json`, iterate `manifest.disks`, remove per-disk dirs. Fall back gracefully if manifest is missing (broken backup) or disk dirs don't exist.

### 5. `upload()` delete_local path -- /Users/rafael.siqueira/dev/personal/chbackup/src/upload/mod.rs:989

```rust
std::fs::remove_dir_all(backup_dir)
```

**Issue:** This only removes the default `backup_dir`. Must also remove per-disk backup dirs when `delete_local` is true.

### 6. `backup::create()` error cleanup -- /Users/rafael.siqueira/dev/personal/chbackup/src/backup/mod.rs:595

```rust
std::fs::remove_dir_all(&backup_dir)
```

**Issue:** Error cleanup only removes the default backup dir. Must also clean per-disk dirs created during the failed backup.

### 7. Restore module shadow path references

**restore/attach.rs lines 293-298 (S3 disk metadata read):**
```rust
let source_dir = p.backup_dir.join("shadow").join(&url_db).join(&url_table).join(&part.name);
```

**restore/attach.rs lines 519-524 (local disk part attach):**
```rust
let source_dir = params.backup_dir.join("shadow").join(&url_db).join(&url_table).join(&part.name);
```

**restore/mod.rs line 987 (ATTACH TABLE mode):**
```rust
let shadow_base = backup_dir.join("shadow").join(&url_db).join(&url_table);
```

All three must be updated to use per-disk paths. The `disk_name` is available via `parts_by_disk` (attach.rs) or from manifest iteration (mod.rs).

### 8. Download module shadow path references

**download/mod.rs line 468-472 (S3 disk metadata download):**
```rust
let shadow_dir = backup_dir.join("shadow").join(&url_db).join(&url_table).join(&item.part.name);
```

**download/mod.rs line 548 (local disk part decompress):**
```rust
let shadow_dir = backup_dir.join("shadow").join(&url_db).join(&url_table);
```

Both must write to per-disk dirs when downloading. The `disk_name` is available from `DownloadWorkItem.disk_name`.

### 9. Hardlink dedup (download) -- find_existing_part / hardlink_existing_part

**download/mod.rs lines 132-192:**
```rust
let candidate = backup_base.join(&name).join("shadow").join(&url_db).join(&url_table).join(part_name);
```

Must also search per-disk backup dirs when looking for existing parts to hardlink.

## Phase 1.5: Call Hierarchy Summary

### Functions that READ from backup_dir/shadow/:
| Function | File | Location | Has disk_name in scope? |
|----------|------|----------|------------------------|
| `find_part_dir()` | upload/mod.rs:1065 | Upload work item construction | Yes (from manifest iteration) |
| `attach_parts_inner()` | restore/attach.rs:519 | Local part attach | No (parts flattened) |
| `restore_s3_disk_parts()` | restore/attach.rs:293 | S3 metadata read | Yes (iterates parts_by_disk) |
| `try_attach_table_mode()` | restore/mod.rs:987 | ATTACH TABLE mode | No (parts flattened) |
| `download()` S3 metadata | download/mod.rs:468 | S3 metadata download | Yes (from work item) |
| `download()` local part | download/mod.rs:548 | Local part decompress | Yes (from work item) |
| `find_existing_part()` | download/mod.rs:132 | Hardlink dedup | No (scanning dirs) |

### Functions that WRITE to backup_dir/shadow/:
| Function | File | Location | Has disk_name in scope? |
|----------|------|----------|------------------------|
| `collect_parts()` hardlink | backup/collect.rs:303 | Backup create | Yes (in disk walk loop) |
| `download()` S3 metadata | download/mod.rs:468 | S3 metadata download | Yes (from work item) |
| `download()` local decompress | download/mod.rs:548 | Local part decompress | Yes (from work item) |

### Functions that DELETE backup_dir/:
| Function | File | Location | Needs manifest for disk paths? |
|----------|------|----------|-------------------------------|
| `delete_local()` | list.rs:477 | Delete local backup | Yes (load manifest.disks) |
| `upload()` delete_local | upload/mod.rs:989 | Post-upload cleanup | Yes (manifest is in memory) |
| `create()` error cleanup | backup/mod.rs:595 | Failed backup cleanup | Yes (disk_map is in scope) |

## Backward Compatibility Analysis

### Old backups (pre per-disk-dirs):
- **Upload:** `find_part_dir()` should fall back to `{data_path}/backup/{name}/shadow/` if part not found in per-disk dir. Already has a fallback pattern.
- **Restore:** `attach_parts_inner()` should fall back to `backup_dir/shadow/` if per-disk path doesn't exist.
- **Delete:** `delete_local()` should only delete per-disk dirs if they exist (non-fatal if missing).
- **Download:** Downloads write to per-disk dirs based on manifest.disks. Old manifests with `disks: {}` will use default data_path only.

### Single-disk setups:
- When only the "default" disk exists, `disk_path == data_path`, so per-disk dir == existing backup_dir. **Zero behavior change.**

### manifest.disks availability:
- `manifest.disks` is populated during `backup::create()` and persisted in `metadata.json`.
- Available at upload time (loaded from local manifest).
- Available at download time (loaded from remote manifest).
- Available at restore time (loaded from local manifest after download).
- Available at delete time (can load from local manifest before deletion).
- For `clean_broken_local()`: manifest may be missing/corrupt. Fall back to default dir only.
