# Type Verification Table

## Key Types

| Variable/Field | Assumed Type | Actual Type | Verification Location |
|---|---|---|---|
| `config.clickhouse.data_path` | `String` | `String` | src/config.rs:117 |
| `BackupManifest.disks` | `HashMap<String, String>` | `HashMap<String, String>` | src/manifest.rs:52 |
| `BackupManifest.disk_types` | `HashMap<String, String>` | `HashMap<String, String>` | src/manifest.rs:56 |
| `TableManifest.parts` | `HashMap<String, Vec<PartInfo>>` | `HashMap<String, Vec<PartInfo>>` | src/manifest.rs:112 (key = disk name) |
| `DiskRow.name` | `String` | `String` | src/clickhouse/client.rs:53 |
| `DiskRow.path` | `String` | `String` | src/clickhouse/client.rs:54 |
| `DiskRow.disk_type` | `String` | `String` | src/clickhouse/client.rs:56 |
| `DiskRow.remote_path` | `String` | `String` | src/clickhouse/client.rs:59 |
| `CollectedPart.disk_name` | `String` | `String` | src/backup/collect.rs:100 |
| `CollectedPart.part_info` | `PartInfo` | `PartInfo` | src/backup/collect.rs:98 |
| `PartInfo.s3_objects` | `Option<Vec<S3ObjectInfo>>` | `Option<Vec<S3ObjectInfo>>` | src/manifest.rs:153 |
| `collect_parts()` return | `Result<HashMap<String, Vec<CollectedPart>>>` | Confirmed | src/backup/collect.rs:126 |
| `hardlink_dir()` signature | `fn(src: &Path, dst: &Path, skip_proj: &[String]) -> Result<()>` | Confirmed | src/backup/collect.rs:403 |
| `find_part_dir()` signature | `fn(backup_dir: &Path, db: &str, table: &str, part_name: &str) -> Result<PathBuf>` | Confirmed | src/upload/mod.rs:1065 |
| `delete_local()` signature | `pub fn(data_path: &str, backup_name: &str) -> Result<()>` | Confirmed | src/list.rs:477 |
| `object_disk::is_s3_disk()` | `fn(&str) -> bool` | Confirmed | src/object_disk.rs (checks "s3" or "object_storage") |
| `disk_map` in create() | `HashMap<String, String>` | `HashMap<String, String>` | src/backup/mod.rs:136-139 (name -> path) |
| `disk_type_map` in create() | `HashMap<String, String>` | `HashMap<String, String>` | src/backup/mod.rs:140-143 (name -> type) |

## Key Function Signatures

### collect_parts (backup/collect.rs:116-126)
```rust
pub fn collect_parts(
    data_path: &str,
    freeze_name: &str,
    backup_dir: &Path,          // <-- THIS is where hardlinks go
    tables: &[TableRow],
    disk_type_map: &HashMap<String, String>,
    disk_paths: &HashMap<String, String>,
    skip_disks: &[String],
    skip_disk_types: &[String],
    skip_projections: &[String],
) -> Result<HashMap<String, Vec<CollectedPart>>>
```

### hardlink_dir (backup/collect.rs:403)
```rust
fn hardlink_dir(src_dir: &Path, dst_dir: &Path, skip_proj_patterns: &[String]) -> Result<()>
```

### find_part_dir (upload/mod.rs:1065)
```rust
fn find_part_dir(backup_dir: &Path, db: &str, table: &str, part_name: &str) -> Result<PathBuf>
```
Resolves to: `{backup_dir}/shadow/{url_db}/{url_table}/{part_name}/`

### delete_local (list.rs:477)
```rust
pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()>
```
Removes: `{data_path}/backup/{backup_name}/`

## Critical Path Analysis

### Hardlink Staging Path (Current)
```
collect_parts():
  staging_dir = backup_dir                       // = {data_path}/backup/{name}
    .join("shadow")
    .join(url_encode_path(&db))
    .join(url_encode_path(&table))
    .join(&part_name)
  hardlink_dir(&shadow_part_path, &staging_dir, skip_projections)
```
`backup_dir` is ALWAYS `{data_path}/backup/{name}`, regardless of which disk the part is on.

### Per-Disk Staging Path (Proposed)
```
collect_parts():
  per_disk_backup_dir = PathBuf::from(disk_path)  // e.g., /mnt/store1
    .join("backup")
    .join(backup_name)
  staging_dir = per_disk_backup_dir
    .join("shadow")
    .join(url_encode_path(&db))
    .join(url_encode_path(&table))
    .join(&part_name)
  hardlink_dir(&shadow_part_path, &staging_dir, skip_projections)
```

### Consumers of Staging Path
1. **upload/find_part_dir** -- must resolve per-disk path using manifest.disks
2. **restore/attach.rs** -- source_dir = `backup_dir.join("shadow")...` -- must resolve per-disk
3. **restore/mod.rs** -- shadow_base = `backup_dir.join("shadow")...` -- must resolve per-disk
4. **download/mod.rs** -- writes to `backup_dir.join("shadow")...` -- must write to per-disk
5. **list/delete_local** -- must remove all per-disk backup dirs
