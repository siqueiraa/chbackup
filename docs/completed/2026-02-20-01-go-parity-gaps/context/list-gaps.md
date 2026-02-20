# Go Parity Gaps: List, Delete, Clean, and Retention Operations

Comparison of `src/list.rs` (Rust) against `pkg/backup/list.go`, `pkg/backup/delete.go`,
`pkg/storage/general.go`, `pkg/storage/utils.go`, and `pkg/backup/utils.go` (Go).

---

## 1. BackupSummary / BackupInfo Field Differences

### Go `BackupInfo` (CLI list output)
```go
type BackupInfo struct {
    BackupName     string
    CreationDate   time.Time
    Size           string      // human-readable multi-component string
    Description    string      // DataFormat + Tags + Broken reason
    RequiredBackup string      // "+base_name" for incrementals
    Type           string      // "local" or "remote"
}
```

### Go `backupJSON` (API list response)
```go
type backupJSON struct {
    Name                string `json:"name"`
    Created             string `json:"created"`
    Size                uint64 `json:"size,omitempty"`
    DataSize            uint64 `json:"data_size,omitempty"`
    ObjectDiskSize      uint64 `json:"object_disk_size,omitempty"`
    MetadataSize        uint64 `json:"metadata_size"`
    RBACSize            uint64 `json:"rbac_size,omitempty"`
    ConfigSize          uint64 `json:"config_size,omitempty"`
    NamedCollectionSize uint64 `json:"named_collection_size,omitempty"`
    CompressedSize      uint64 `json:"compressed_size,omitempty"`
    Location            string `json:"location"`
    RequiredBackup      string `json:"required"`
    Desc                string `json:"desc"`
}
```

### Rust `BackupSummary` (both CLI and as basis for API)
```rust
pub struct BackupSummary {
    pub name: String,
    pub timestamp: Option<DateTime<Utc>>,
    pub size: u64,              // total uncompressed
    pub compressed_size: u64,
    pub table_count: usize,
    pub metadata_size: u64,
    pub is_broken: bool,
    pub broken_reason: Option<String>,
}
```

### Gaps

| Field | Go | Rust | Status |
|-------|-----|------|--------|
| `required_backup` | Shows `+base_name` from `BackupMetadata.RequiredBackup` | **MISSING** from `BackupSummary` | **GAP** -- not shown in CLI list output or included in summary struct |
| `data_format` / description | Shows `DataFormat + Tags` in description column | Not shown anywhere in list output | **MINOR GAP** -- we show table_count instead |
| `object_disk_size` | Tracked in `BackupMetadata.ObjectDiskSize`, shown in size string and API | Not tracked in manifest or summary | **GAP** -- manifest lacks `object_disk_size` field |
| `rbac_size` | Tracked in `BackupMetadata.RBACSize` | Hardcoded `0` in API response | **KNOWN** -- documented as limitation |
| `config_size` | Tracked in `BackupMetadata.ConfigSize` | Hardcoded `0` in API response | **KNOWN** -- documented as limitation |
| `named_collection_size` | Tracked in `BackupMetadata.NamedCollectionsSize`, shown in API | **MISSING** from both manifest and API response struct | **GAP** -- `ListResponse` lacks `named_collection_size` field |
| `type` / `location` | Shows "local" or "remote" in each row | Only shown as section header ("Local backups:") in default mode | **MINOR GAP** -- structured formats don't include location |
| `tags` | Appended to description | Not tracked | **MINOR GAP** -- tags feature not implemented |
| `data_size` | `BackupMetadata.DataSize` (uncompressed data only) | We use `total_bytes` sum as both `size` and `data_size` | **ACCEPTABLE** -- semantically same for S3-only |

---

## 2. Backup Size String Format

### Go CLI format (`getBackupSizeString`)
```
all:1.23GiB,data:1.00GiB,arch:500.00MiB,obj:200.00MiB,meta:1.00KiB,rbac:0B,conf:0B,nc:0B
```

Uses `FormatBytes()` with binary units (KiB, MiB, GiB, TiB) and multi-component output
showing all 8 size categories in a single comma-separated string.

Broken backups show `???` for size.

### Rust CLI format
```
  backup-name  2024-01-15 12:00:00 UTC  1.00 GB  512.00 KB  3 tables
```

Uses `format_size()` with non-standard units (KB, MB, GB, TB using 1024 base but
non-IEC names) and shows only `size` + `compressed_size` + `table_count`.

### Gaps

1. **Unit naming**: Go uses IEC binary units (KiB, MiB, GiB). Rust uses ambiguous names
   (KB, MB, GB). **LOW PRIORITY** -- cosmetic difference.

2. **Multi-component size string**: Go shows all 8 size components inline. Rust shows only
   total + compressed. The Go format is more informative but also more verbose.
   **MINOR GAP** -- our format is simpler but functional.

3. **Broken backup size**: Go shows `???`. Rust shows `0 B` (size=0 for broken backups).
   **MINOR GAP** -- functional difference in how broken backups display.

---

## 3. List Sort Order

### Go
- **Local**: Sorted by `CreationDate` ascending (oldest first).
- **Remote**: `BackupList()` sorts first by `BackupName` ascending, then by
  `UploadDate` ascending (stable sort, so UploadDate is primary within same name).
- **API**: Same order as underlying `BackupList()` / `GetLocalBackups()`.

### Rust
- **Local**: Sorted by `name` (alphabetical) ascending.
- **Remote**: Sorted by `name` (alphabetical) ascending.
- **API**: `desc` query param reverses sort when true.

### Gap
**MINOR**: Rust sorts by name while Go sorts by date. For date-based backup names
(the common case), these produce the same order. For non-date names, the order
could differ. Not a functional issue in practice.

---

## 4. Latest/Previous Shortcuts

### Go
Accepts `ptype` parameter with aliases:
- `"latest"`, `"last"`, `"l"` -- last backup
- `"penult"`, `"prev"`, `"previous"`, `"p"` -- second-to-last
- `"all"`, `""` -- all backups

Shortcuts work on **both** local and remote independently. Broken backups ARE
included (no filtering). Returns raw data from the sorted backup list.

### Rust
`resolve_backup_shortcut()` accepts:
- `"latest"` -- last non-broken backup
- `"previous"` -- second-to-last non-broken backup

Operates on a pre-filtered list (broken backups excluded). Does **not** accept
the Go aliases (`"last"`, `"l"`, `"penult"`, `"prev"`, `"p"`).

### Gaps

1. **Missing aliases**: Rust does not accept `"last"`, `"l"`, `"penult"`, `"prev"`,
   `"p"` as shortcuts. **LOW PRIORITY** -- our naming is clear, but users migrating
   from Go might expect the aliases.

2. **Broken backup inclusion**: Go includes broken backups in latest/previous
   resolution. Rust excludes them. **ACCEPTABLE** -- our behavior is arguably better
   (returning a usable backup), but differs from Go.

---

## 5. Delete Logic

### Go `RemoveBackupLocal`
1. Sanitizes backup name via `CleanBackupNameRE`
2. Connects to ClickHouse to get disk list
3. Checks for object disk data in the backup
4. If object disks present AND same backup is NOT on remote, cleans object disk S3 data
5. Removes backup directory from ALL disks (iterates through all disks, not just default)
6. Supports embedded backup disk cleanup

### Rust `delete_local`
1. Constructs path `{data_path}/backup/{backup_name}/`
2. Checks existence
3. Calls `remove_dir_all()`

### Gaps

1. **Multi-disk support**: Go iterates through ALL ClickHouse disks and removes
   backup data from each disk path. Rust only checks `{data_path}/backup/`.
   **GAP** -- if backups span multiple local disks, Rust would leave orphaned data
   on non-default disks.

2. **Object disk cleanup on local delete**: Go checks whether to clean S3 object
   disk data (only if the same backup is not also present on remote). Rust does not
   perform any S3 cleanup when deleting local backups. **GAP** -- could leave
   orphaned S3 objects for object-disk-backed tables.

3. **Backup name sanitization**: Go uses `CleanBackupNameRE` to sanitize. Rust does
   not sanitize. **MINOR GAP** -- could be a security/correctness concern with
   specially crafted names.

### Go `RemoveBackupRemote`
1. Sanitizes backup name
2. Gets backup list, finds the specific backup metadata
3. Cleans embedded/object disk data if same backup NOT present locally
4. Calls `bd.RemoveBackupRemote()` which does batch deletion with retry and
   exponential backoff

### Rust `delete_remote`
1. Lists all objects under `{backup_name}/`
2. Batch-deletes via `s3.delete_objects()`

### Gaps

1. **Object disk cleanup on remote delete**: Go conditionally cleans S3 object disk
   data. Rust does not. Same gap as local delete. **GAP**.

2. **Retry logic**: Go uses retry with exponential backoff for remote deletion. Rust
   does a single attempt. **GAP** -- resilience difference.

3. **Batch deletion size**: Go uses configurable `DeleteBatchSize`. Rust sends all
   keys in a single `delete_objects` call. **MINOR GAP** -- could hit AWS limits for
   very large backups (S3 DeleteObjects max is 1000 keys per request, but our
   `S3Client` may handle batching internally).

---

## 6. Clean Shadow / Clean

### Go `Clean()`
Removes ALL files from shadow directories on ALL non-backup disks. Does NOT filter
by `chbackup_*` prefix -- removes everything in `shadow/`.

### Rust `clean_shadow()`
Removes only `chbackup_*` directories from shadow paths on all non-backup disks.
Has optional name filter for targeted cleanup.

### Gap
**BEHAVIORAL DIFFERENCE**: Go's `Clean()` removes ALL shadow contents (including
non-chbackup freezes). Rust only removes `chbackup_*` prefixed directories. This is
actually **safer** behavior in our implementation -- we don't destroy other tools'
shadow data. **ACCEPTABLE** -- our behavior is more conservative and correct.

---

## 7. Clean Broken

### Go `CleanLocalBroken` / `CleanRemoteBroken`
- Iterates all backups, deletes those with `Broken != ""`
- Uses full `RemoveBackupLocal`/`RemoveBackupRemote` (which includes object disk
  cleanup, multi-disk cleanup, etc.)
- Errors are **fatal** -- returns on first error

### Rust `clean_broken_local` / `clean_broken_remote`
- Lists all backups, filters broken, deletes each
- Uses simpler `delete_local`/`delete_remote`
- Errors on individual backups are **warnings** (continues to next)
- Returns count of deleted

### Gap
1. **Error handling**: Go stops on first error, Rust continues (our behavior is more
   resilient). **ACCEPTABLE**.
2. **Object disk cleanup**: Go's `RemoveBackupLocal` handles multi-disk and object
   disk cleanup. Rust's `delete_local` does not. **GAP** (same as delete).

---

## 8. Retention (Local)

### Go `RemoveOldBackupsLocal`
```go
keep := b.cfg.General.BackupsToKeepLocal
if keep == 0 { return nil }
if keep < 0 { keep = 0; if keepLastBackup { keep = 1 } }
```
- Uses `GetBackupsToDeleteLocal()` which sorts by `CreationDate` descending and
  returns `backups[keep:]`
- Does NOT filter out broken backups -- they count toward the total and can be
  deleted
- `keep < 0` + `keepLastBackup=true` means "keep only 1" (used after upload)
- Uses full `RemoveBackupLocal` for deletion

### Rust `retention_local`
```rust
if keep <= 0 { return Ok(0); }
// Filter to valid (non-broken) backups only
```
- Filters out broken backups from counting
- Sorts by timestamp ascending
- Uses simple `delete_local` for deletion

### Gaps

1. **Broken backup handling**: Go includes broken backups in retention counting and
   deletes them. Rust excludes broken backups entirely. **BEHAVIORAL DIFFERENCE** --
   Go's approach means a backup slot can be "wasted" on a broken backup, but broken
   backups also get cleaned up automatically. Rust's approach is cleaner but means
   broken backups can accumulate unless `clean_broken` is called separately.

2. **`keep < 0` handling**: Go treats `keep < 0` specially (keep only last backup
   when `keepLastBackup=true`). Rust treats `keep <= 0` uniformly as "no retention".
   **MINOR GAP** -- the `keep=-1` case is used by the upload pipeline's
   "delete after upload" logic.

3. **Config resolution**: Go has `BackupsToKeepLocal` only in `GeneralConfig` (no
   separate retention section). Rust has both `retention.backups_to_keep_local` and
   `general.backups_to_keep_local` with override logic in `effective_retention_local()`.
   **NOT A GAP** -- our implementation is more flexible.

---

## 9. Retention (Remote)

### Go `RemoveOldBackupsRemote`
- Only runs if `BackupsToKeepRemote >= 1`
- Gets backup list, calls `GetBackupsToDeleteRemote()`:
  1. Sorts by `UploadDate` descending
  2. Splits into keep (first `keep`) and delete (remainder)
  3. **Incremental chain protection**: Recursively traces `RequiredBackup` field.
     For each kept backup, follows the chain: if a to-delete backup is referenced
     by a kept backup's `RequiredBackup`, it is removed from the delete list
  4. Filters out backups with zero `UploadDate` (race condition protection for
     multi-shard copy)
- Deletion uses `bd.RemoveBackupRemote()` with retry and batch deletion

### Rust `retention_remote`
- Only runs if `keep > 0`
- Lists remote backups, filters broken, sorts by timestamp ascending
- **Incremental chain protection**: Scans surviving backup manifests for
  `carried:{base_name}` patterns in `PartInfo.source`. Skips deletion of any
  backup referenced as an incremental base.
- Per-deletion GC: calls `gc_collect_referenced_keys()` fresh for each deletion
  to build set of all S3 keys referenced by surviving backups
- Uses `gc_delete_backup()` which partitions keys into referenced/unreferenced
  and only deletes unreferenced keys (manifest last)

### Gaps

1. **Incremental chain protection mechanism**: Go uses `BackupMetadata.RequiredBackup`
   field (a direct pointer to the base backup name). Rust uses `carried:{name}` pattern
   in `PartInfo.source`. Both achieve the same goal but through different data structures.
   **ACCEPTABLE** -- Rust approach is manifest-driven and doesn't require a top-level
   `RequiredBackup` field.

2. **Zero UploadDate filter**: Go filters out backups with `0001-01-01 00:00:00`
   UploadDate to avoid race conditions during multi-shard operations. Rust does not
   have this filter. **LOW PRIORITY** -- relevant only for multi-shard concurrent
   operations which are edge cases.

3. **GC behavior**: Go's `RemoveBackupRemote()` deletes ALL objects under the backup
   prefix (no GC filtering). Rust's `gc_delete_backup()` preserves keys referenced
   by other surviving backups. **Rust is MORE correct** -- Go can delete shared
   objects used by incremental backups when the base is deleted. Rust's per-key GC
   prevents this.

4. **Broken backup handling**: Same as local retention -- Go includes broken, Rust
   excludes.

---

## 10. Format Output

### Go
- **text**: `tabwriter` with tab-separated columns: Name, CreationDate (local TZ),
  Type, RequiredBackup, Size (multi-component string), Description
- **json**: `json.Marshal` of `[]BackupInfo`
- **yaml**: `yaml.Marshal` of `[]BackupInfo`
- **csv**: `gocsv.MarshalString` of `[]BackupInfo` (uses struct field names as headers)
- **tsv**: Same as CSV but with tab delimiter

### Rust
- **text**: Manual format with Name, Status, Timestamp (UTC), Size, CompressedSize,
  TableCount
- **json**: `serde_json::to_string_pretty` of `&[BackupSummary]`
- **yaml**: `serde_yaml::to_string` of `&[BackupSummary]`
- **csv/tsv**: Manual delimiter-separated with explicit header row

### Gaps

1. **Timestamp timezone**: Go uses local timezone, Rust uses UTC. **MINOR GAP** --
   UTC is more portable but differs from Go output.

2. **Field set difference**: Go text output has Type and RequiredBackup columns.
   Rust has CompressedSize and TableCount columns. Different information emphasis.
   **ACCEPTABLE** -- Rust shows more useful size info.

3. **CSV/TSV headers**: Go uses struct field names via gocsv (e.g., "BackupName",
   "CreationDate"). Rust uses snake_case headers (e.g., "name", "timestamp").
   **MINOR GAP** -- header naming differs but functionality is equivalent.

4. **JSON pretty printing**: Rust uses `to_string_pretty` (indented). Go uses
   `json.Marshal` (compact). **COSMETIC** difference.

---

## 11. Local Backup Discovery (Multi-Disk)

### Go `GetLocalBackups`
- Queries ClickHouse for all disks
- Iterates through ALL disk paths, looking for `{disk.Path}/backup/` (or just
  `{disk.Path}/` for backup-type disks)
- Deduplicates by backup name (if same backup seen on multiple disks, metadata
  from later disk overwrites the "broken" placeholder from first disk)

### Rust `list_local`
- Only checks `{data_path}/backup/`

### Gap
**GAP**: Rust only discovers backups on the default data path. Go discovers backups
across all ClickHouse disks. For multi-disk setups, Rust would miss backups stored
on non-default disks.

---

## 12. API List Response Differences

### Go `backupJSON`
Includes: `named_collection_size`, `desc` (description string with data_format +
tags + broken reason), all size fields populated from `BackupMetadata`.

### Rust `ListResponse`
Missing: `named_collection_size` field entirely. `object_disk_size` hardcoded to 0.
No `desc` field (broken info is in `is_broken` / `broken_reason` but not in API
response).

### Gaps

1. **Missing `named_collection_size`**: Not in `ListResponse` struct. **GAP** for
   API parity.
2. **Missing `desc` field**: Go returns a description combining data_format, broken
   status, and tags. Rust's `ListResponse` has no description/desc field. **GAP**.
3. **`object_disk_size` always 0**: Comment in code says "Requires manifest
   disk_types analysis (future)". **KNOWN LIMITATION**.

---

## 13. Manifest-Level Size Fields

### Go `BackupMetadata`
```go
MetadataSize        uint64
ConfigSize          uint64
RBACSize            uint64
DataSize            uint64
ObjectDiskSize      uint64
CompressedSize      uint64
NamedCollectionsSize uint64
RequiredBackup      string    // name of incremental base
Tags                string    // comma-separated tags
DataFormat          string
```

### Rust `BackupManifest`
```rust
pub compressed_size: u64,
pub metadata_size: u64,
pub data_format: String,
// per-table: total_bytes (uncompressed)
```

### Gap
Manifest is missing these Go-equivalent fields:
- `data_size` (aggregate across all tables -- Rust computes this dynamically)
- `object_disk_size` -- **GAP**: never computed
- `rbac_size` -- **GAP**: never computed
- `config_size` -- **GAP**: never computed
- `named_collections_size` -- **GAP**: never computed
- `required_backup` -- **GAP**: no top-level incremental base pointer
- `tags` -- **GAP**: tags feature not implemented

The dynamic computation of `data_size` (sum of `total_bytes`) is acceptable.
The missing fields mean size breakdown in API responses is incomplete.

---

## Summary of Actionable Gaps (Ordered by Impact)

### HIGH

| # | Gap | Impact |
|---|-----|--------|
| H1 | Multi-disk local backup discovery: `list_local` only checks `data_path`, misses backups on other disks | Users with multi-disk ClickHouse setups would see incomplete backup lists |
| H2 | Multi-disk local delete: `delete_local` only removes from `data_path`, leaves orphaned data on other disks | Disk space leak on multi-disk setups |
| H3 | No object disk cleanup on local/remote delete | S3 object disk data orphaned when backup deleted |

### MEDIUM

| # | Gap | Impact |
|---|-----|--------|
| M1 | `BackupSummary` lacks `required_backup` field | CLI list and API response don't show incremental dependency info |
| M2 | Manifest lacks `object_disk_size` field | Size reporting incomplete for object disk backups |
| M3 | API `ListResponse` missing `named_collection_size` field | API schema mismatch with Go |
| M4 | API `ListResponse` missing `desc` field | API schema mismatch with Go |
| M5 | Remote delete has no retry logic | Less resilient to transient S3 errors |
| M6 | Broken backups included in Go retention but excluded in Rust | Different retention counting behavior for mixed broken/valid backup sets |

### LOW

| # | Gap | Impact |
|---|-----|--------|
| L1 | Missing latest/previous aliases (`last`, `l`, `penult`, `prev`, `p`) | Minor convenience for migrating users |
| L2 | Timestamp in local timezone (Go) vs UTC (Rust) | Cosmetic difference in output |
| L3 | Size unit naming (KiB/MiB/GiB vs KB/MB/GB) | Cosmetic difference |
| L4 | Multi-component size string vs simple size columns | Different output style |
| L5 | Zero UploadDate filter in remote retention missing | Edge case for multi-shard concurrent ops |
| L6 | Backup name sanitization missing | Minor security/correctness concern |
| L7 | Go `Clean()` removes all shadow contents vs Rust only `chbackup_*` | Rust is actually MORE correct |
