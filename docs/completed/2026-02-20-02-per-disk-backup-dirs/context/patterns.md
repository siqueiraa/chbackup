# Pattern Discovery

## Global Patterns Registry
No global pattern files exist at `docs/patterns/*.md`. Full local discovery performed.

## Component Identification

### Components Modified
1. **backup/collect.rs** - `collect_parts()` + `hardlink_dir()` -- hardlink destination changes from single backup_dir to per-disk dir
2. **backup/mod.rs** - `create()` -- backup_dir construction, error cleanup
3. **upload/mod.rs** - `find_part_dir()` -- locating part data for upload
4. **restore/attach.rs** - `attach_parts()` source path -- reading part data from backup for restore
5. **restore/mod.rs** - `restore_attach_table_mode()` source path -- same
6. **list.rs** - `delete_local()` -- must clean up per-disk backup dirs too
7. **download/mod.rs** - Download decompresses to `backup_dir/shadow/...` -- must write to per-disk dirs
8. **manifest.rs** - `BackupManifest.disks` already stores disk->path mapping (no change needed)

### Patterns Discovered

#### Pattern 1: Backup Directory Layout (Current)
```
{data_path}/backup/{backup_name}/
  metadata.json
  metadata/{db}/{table}.json
  shadow/{url_db}/{url_table}/{part_name}/...   <-- ALL local parts go here
  access/...
  configs/...
```
**Problem:** All hardlinked parts go under `{data_path}/backup/`, which is on the default disk. Parts from other NVMe disks trigger EXDEV -> copy fallback.

#### Pattern 2: Per-Disk Backup Directory Layout (Target -- Go clickhouse-backup compatible)
```
{data_path}/backup/{backup_name}/           <-- metadata + manifest (always on default disk)
  metadata.json
  metadata/{db}/{table}.json
  access/...
  configs/...

{disk_path}/backup/{backup_name}/           <-- per-disk shadow data
  shadow/{url_db}/{url_table}/{part_name}/...
```
For the "default" disk, `{disk_path}` == `{data_path}`, so the layout is unchanged.
For non-default disks (e.g., store0, store1), shadow data lives on that disk's mount point.

#### Pattern 3: Disk Iteration Pattern (collect_parts)
```rust
// Already iterates all disks:
for (disk_name, disk_path) in &paths_to_walk {
    // Skip excluded disks
    // Walk shadow/{freeze_name}/store/...
    // For local parts: hardlink_dir(shadow_part, staging_dir, ...)
    // For S3 parts: parse metadata (no hardlink)
}
```
The key change is in `staging_dir` computation: currently always `backup_dir.join("shadow")...`, needs to become `per_disk_backup_dir.join("shadow")...`.

#### Pattern 4: Part Location Resolution (upload/find_part_dir)
Currently: `{backup_dir}/shadow/{url_db}/{url_table}/{part_name}/`
Needs to search across disk-specific backup dirs.

#### Pattern 5: Delete Cleanup Pattern (list/delete_local)
Currently: `remove_dir_all({data_path}/backup/{backup_name}/)`
Needs to also remove `{disk_path}/backup/{backup_name}/` for each non-default disk.

#### Pattern 6: Manifest Already Has Disk Info
`BackupManifest.disks: HashMap<String, String>` -- maps disk name to disk path.
`TableManifest.parts: HashMap<String, Vec<PartInfo>>` -- keys are disk names.
This means we can determine which disks were used from the manifest at upload/restore/delete time.

#### Pattern 7: Download Creates Backup Dir
Download decompresses to `{data_path}/backup/{backup_name}/shadow/{db}/{table}/{part_name}/...`
For per-disk, download needs to check `manifest.disks` to write parts to the correct disk path.
However, downloaded backups are always on the same host with same disk layout OR on a different host where all disks may not exist. Key insight: download output is consumed by restore, which reads the manifest to find the correct path.
