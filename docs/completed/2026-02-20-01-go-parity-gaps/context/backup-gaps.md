# Backup Create Flow: Go vs Rust Parity Analysis

Comparison of `Altinity/clickhouse-backup` (Go, `pkg/backup/create.go` + helpers) against
`chbackup` (Rust, `src/backup/` module). Covers the full `create` command flow:
FREEZE, shadow walk, hardlink, manifest creation.

---

## 1. High-Level Flow Comparison

### Go Flow (`CreateBackup` -> `createBackupLocal`)
1. PID lock check
2. Connect to ClickHouse, get version
3. Skip databases with unsupported engines (MySQL, PostgreSQL, MaterializedPostgreSQL)
4. Get tables with `GetTables()` (filters by pattern, skip_tables, skip_table_engines, handles `.inner_id.`/`.inner.` enrichment)
5. Get user-defined functions
6. Get disks, default data path
7. Convert partition arguments to ID map
8. RBAC/config/named-collections backup (unconditionally before table processing)
9. `CheckPartsColumns` if enabled (bulk check, returns error if inconsistent)
10. Parallel table processing via `errgroup`:
    - For each table: `AddTableToLocalBackup()` which does FREEZE (with inline SYNC REPLICA) -> shadow walk per disk -> hardlink/link -> checksum -> object disk upload -> UNFREEZE
11. In-progress mutations captured per table
12. Table metadata written per table (inside parallel group)
13. `createBackupMetadata()` writes top-level `metadata.json`
14. On error: `RemoveBackupLocal()` + `Clean()` (shadow cleanup)
15. `RemoveOldBackupsLocal()` for local retention

### Rust Flow (`create()`)
1. Get ClickHouse version
2. Get disk information
3. List all tables, query dependencies (CH 23.3+)
4. Filter tables by pattern, skip_tables, skip_table_engines
5. Check `allow_empty_backups`
6. Parts column consistency check (with benign type drift filtering)
7. JSON/Object column type detection (warning only)
8. Check pending mutations
9. Sync replicas (if `sync_replicated_tables`)
10. Create backup directory
11. Parallel FREEZE + collect via `tokio::spawn` + Semaphore:
    - For each table: FREEZE (whole or per-partition) -> `collect_parts()` via `spawn_blocking` -> CRC64 -> build TableManifest
12. UNFREEZE all tables (via FreezeGuard)
13. On error: remove backup dir + clean shadow
14. Build database list, save per-table metadata
15. Build manifest, backup RBAC/configs/named-collections/functions
16. Apply incremental diff if `--diff-from`
17. Save manifest

---

## 2. FREEZE Logic

### 2.1 SYNC REPLICA Timing

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| When SYNC REPLICA runs | Inline in `FreezeTable()` -- immediately before FREEZE for each table | Batch: `sync_replicas()` runs before any FREEZE | **Different approach, not a bug** |
| Engine check | `strings.HasPrefix(table.Engine, "Replicated")` | `t.engine.contains("Replicated")` | Equivalent |
| On error | `log.Warn()` + continue | `warn!()` + continue | Match |

**Assessment**: Go does SYNC REPLICA right before each table's FREEZE (tighter coupling), while Rust syncs all replicated tables upfront. Both approaches are valid. Go's approach is slightly more correct for long-running backup operations where data could change between sync and freeze. This is a minor behavioral difference, not a correctness bug.

### 2.2 FREEZE Error Handling

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Error codes checked | 60 (UNKNOWN_TABLE), 81 (UNKNOWN_DATABASE), 218 (CANNOT_FREEZE_PARTITION) | Same: 60, 81, 218 | Match |
| Config gate | `IgnoreNotExistsErrorDuringFreeze` | `ignore_not_exists_error_during_freeze` | Match |
| Silent return on ignorable | `return nil` (Go FreezeTable returns nil, table appears as having no parts) | Returns `Ok(None)`, table skipped from manifest | Match |

### 2.3 Freeze-by-Part (Partition-Level FREEZE)

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Trigger | `ch.Config.FreezeByPart` or CH version < 19.1 | `config.clickhouse.freeze_by_part` or `--partitions` CLI | Match (version check not needed since we target 21.8+) |
| WHERE filter | `ch.Config.FreezeByPartWhere` appended to partition discovery query | `config.clickhouse.freeze_by_part_where` | Match |
| Special partition "all" | `FREEZE PARTITION tuple()` for `partition_id = "all"` | Returns empty vec -> whole-table FREEZE | **GAP**: Go explicitly handles `partition_id = "all"` with `FREEZE PARTITION tuple()`. Rust treats "all" as "do whole-table freeze" which is semantically equivalent but via a different mechanism |
| Error on partition freeze | Codes 60/81 with `IgnoreNotExistsErrorDuringFreeze` | Codes 60/81/218 with same config | Match |

### 2.4 Engine Filtering for FREEZE

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Engines that get data backup | `*MergeTree`, `MaterializedMySQL`, `MaterializedPostgreSQL` | Any engine not in `is_metadata_only_engine()` | **GAP** |
| `is_metadata_only_engine` | N/A -- Go checks `strings.HasSuffix(table.Engine, "MergeTree")` at AddTableToLocalBackup | `View`, `MaterializedView`, `LiveView`, `WindowView`, `Dictionary`, `Null`, `Set`, `Join`, `Buffer`, `Distributed`, `Merge` | See below |

**GAP DETAIL**: Go's `AddTableToLocalBackup()` explicitly checks:
```go
if !strings.HasSuffix(table.Engine, "MergeTree") && table.Engine != "MaterializedMySQL" && table.Engine != "MaterializedPostgreSQL" {
    // supports only schema backup
    return nil, nil, nil, nil, nil
}
```
This means engines like `Memory`, `File`, `URL`, `MySQL` (engine, not database), `PostgreSQL`, `Kafka`, `RabbitMQ`, `S3Queue`, `NATS`, `HDFS` are implicitly schema-only in Go.

Rust's `is_metadata_only_engine()` only matches a specific list. Engines like `Memory`, `File`, `URL`, `Kafka`, `RabbitMQ`, `S3Queue`, etc. would **attempt FREEZE** in Rust, which would likely fail or produce empty shadow directories. The Go approach is safer because it whitelists data-capable engines rather than blacklisting schema-only ones.

**Severity: MEDIUM** -- FREEZE on a `Memory` or `Kafka` engine table will likely produce an error or empty result, but it wastes time and may produce confusing error messages.

### 2.5 Embedded Backup Mode

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| `use_embedded_backup_restore` config | Full implementation via `createBackupEmbedded()` using native `BACKUP ... TO ...` SQL | Not implemented | **INTENTIONAL OMISSION** per design doc (S3-only, streaming by default) |

---

## 3. Shadow Walk

### 3.1 Shadow Path Formats

Go handles TWO shadow path formats:
```
store / {3char_prefix} / {uuid} / {part_name} / {files}     (modern CH, Atomic engine)
data  / {database}     / {table} / {part_name} / {files}     (old CH, Ordinary engine)
```

The Go `MoveShadowToBackup` function uses `strings.SplitN(relativePath, "/", 4)` which splits on the 4th component -- in both cases `pathParts[3]` gets the `part_name/file` portion. It does NOT attempt to resolve which table a `store/prefix/uuid` part belongs to (it already knows because it passed a specific table to `FreezeTable` and uses a unique `shadowBackupUUID`).

**Key difference**: Go uses a random UUID for the shadow backup name (`shadowBackupUUID := strings.ReplaceAll(uuid.New().String(), "-", "")`), NOT a deterministic name. This means each table gets its own unique shadow directory.

Rust uses a deterministic freeze name: `chbackup_{backup}_{db}_{table}` and then does a post-hoc UUID-to-table mapping via `build_uuid_map()`.

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Shadow name | Random UUID per table | Deterministic `chbackup_{backup}_{db}_{table}` | Different but both correct |
| Table identification | Implicit (each table has its own shadow UUID) | UUID map from `system.tables.data_paths` | Different approach |
| `data/db/table` path format | Handled via `SplitN(_, "/", 4)` | **NOT HANDLED** -- only walks `store/prefix/uuid/` pattern | **GAP** |

**GAP DETAIL**: Rust's `collect_parts()` explicitly iterates `store/{prefix_3}/{uuid_dir}/{part_name}` which only handles the `store/` format. Tables using the old Ordinary database engine produce shadow paths like `data/{database}/{table}/{part_name}/`. This would be silently missed.

**Severity: LOW** -- The Ordinary database engine is deprecated since CH 20.x and rare in practice. Our minimum CH version is 21.8, where Atomic is the default. However, for completeness, the `data/` path format should be supported.

### 3.2 Shadow Walk Per Disk

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Walk all disks | Yes, iterates `diskList` in `AddTableToLocalBackup` | Yes, iterates `paths_to_walk` in `collect_parts` | Match |
| Default data path fallback | Implicit via `disk.Path` always including default disk | Explicit: adds `data_path` if not in `disk_paths` | Match |
| Disk type detection | `isDiskTypeObject()`: "s3", "azure_blob_storage", "azure" | `is_s3_disk()`: "s3", "object_storage" | **GAP** |

**GAP DETAIL**: Go's `isDiskTypeObject()` includes Azure blob storage types:
```go
func (b *Backuper) isDiskTypeObject(diskType string) bool {
    return diskType == "s3" || diskType == "azure_blob_storage" || diskType == "azure"
}
```
Rust only handles S3 types. This is intentional per the design doc (S3-only storage), but the disk type detection for the shadow walk should still recognize `azure` disks even if we don't support them as backup targets.

### 3.3 Encrypted Disk Handling

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Encrypted disk detection | `isDiskTypeEncryptedObject()`: checks if `disk.Type == "encrypted"` and underlying disk is object type | Not implemented | **GAP** |

**GAP DETAIL**: Go has special handling for encrypted disks that wrap object storage disks:
```go
func (b *Backuper) isDiskTypeEncryptedObject(disk clickhouse.Disk, disks []clickhouse.Disk) bool {
    if disk.Type != "encrypted" { return false }
    // Check if underlying disk (by path prefix match) is an object disk
    for _, d := range disks {
        if d.Name != disk.Name && strings.HasPrefix(disk.Path, d.Path) && isDiskTypeObject(d.Type) {
            return true
        }
    }
    return false
}
```
In Go, encrypted object disks are treated the same as plain object disks for backup purposes (CopyObject instead of hardlink). Rust does not detect encrypted disks wrapping object storage.

**Severity: LOW-MEDIUM** -- Encrypted object disks are uncommon but exist in production. Without this detection, encrypted S3 disk parts would be treated as local disk parts, which would fail or produce incorrect backups.

### 3.4 `frozen_metadata.txt` Skipping

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Skip `frozen_metadata.txt` | `strings.Contains(info.Name(), "frozen_metadata.txt")` | `part_name == "frozen_metadata.txt"` at directory level | Match (different level but same effect) |

### 3.5 `checksums.txt` Validation

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Part validation | Directory is a part if it has files at the right depth | Checks for `checksums.txt` existence | Rust is stricter, which is fine |

---

## 4. Hardlink Creation

### 4.1 Hardlink vs Move

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| CH version < 21.4 | `os.Rename()` (move) | Not handled (targets 21.8+) | OK per design |
| CH version >= 21.4 | `os.Link()` (hardlink) | `std::fs::hard_link()` + EXDEV fallback to copy | Match (Rust has extra cross-device fallback) |
| EXDEV fallback | Not implemented (Go returns error) | Falls back to `std::fs::copy()` | **Rust is better** |

### 4.2 Non-Regular File Handling

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Non-regular files | `!info.Mode().IsRegular()` -> skip with debug log | `walkdir` reports file_type, only hardlinks files | Match |

### 4.3 Backup Shadow Directory Layout

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Staging path | `{disk_path}/backup/{backupName}/shadow/{db_encoded}/{table_encoded}/{disk_name}/{part_name}/` | `{data_path}/backup/{backupName}/shadow/{db_encoded}/{table_encoded}/{part_name}/` | **GAP** |

**GAP DETAIL**: Go includes the **disk name** as a path component in the staging directory:
```
backup/{name}/shadow/{db}/{table}/{diskName}/{partName}/
```
Rust omits the disk name:
```
backup/{name}/shadow/{db}/{table}/{partName}/
```

This means that if a table has parts on multiple disks, Go separates them by disk in the filesystem layout, while Rust puts them all in the same directory. This affects the upload module which reads from the staging directory.

However, looking at both codebases more carefully: the Rust `TableManifest.parts` is a `HashMap<String, Vec<PartInfo>>` keyed by disk name, which provides the disk association in the manifest. The Go metadata also uses `Parts map[string][]Part` keyed by disk name.

**Severity: LOW** -- The staging directory layout difference doesn't affect correctness since disk association is maintained in the manifest. But it means the local backup directory layout is not Go-compatible for direct cross-tool usage.

### 4.4 Chown (File Ownership)

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Chown on backup files | Yes, `Chown()` after RBAC/config backup, metadata write | Not during create (only during restore) | **GAP** |

**GAP DETAIL**: Go calls `filesystemhelper.Chown()` on:
1. The backup path after RBAC/config/named-collections backup
2. Each table metadata file after creation
3. The top-level `metadata.json` after creation

This ensures the ClickHouse user can read the backup files. Rust does not chown during `create`, which could cause permission issues if the backup tool runs as root but ClickHouse runs as a different user.

**Severity: LOW** -- The backup create flow typically runs as the ClickHouse user, and the restore flow (where Rust does chown) is where ownership matters most. But for consistency with Go behavior, chown during create would be a nice-to-have.

---

## 5. Skip Projections

### 5.1 Pattern Format

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Pattern format | `[db.]table:projection` or just `projection`, comma-separated in a single string | Glob pattern on projection stem only | **GAP** |

**GAP DETAIL**: Go's `IsSkipProjections()` supports a rich pattern format:
```
db.table:projection_name     -- skip specific projection on specific table
table:projection_name         -- skip specific projection on any db's table
projection_name               -- skip projection with this name on any table
*                             -- skip all projections
```
The pattern is matched against `db/table/part/projection.proj/file` paths using `filepath.Match`.

Rust's `should_skip_projection()` only matches on the projection stem (name without `.proj`):
- `*` matches all projections
- `my_*` matches projections starting with `my_`
- No per-table or per-database scoping

**Severity: LOW** -- The simpler Rust approach covers the most common use cases. Per-table scoping is an advanced feature rarely needed.

### 5.2 Version Warning

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| CH < 24.3 warning | `log.Warn("backup with skip-projections can restore only in 24.3+")` | Not implemented | **MINOR GAP** |

---

## 6. Part Sorting

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Sort after collect | `metadata.SortPartsByMinBlock(parts)` | Not implemented in create flow | **GAP** |
| Sort algorithm | `SplitN(name, "_", 3)` -> sort by partition (lexicographic), then by min_block (numeric) | `parse_part_name()` exists in `collect.rs` but not used for sorting | See below |

**GAP DETAIL**: Go sorts parts by (partition_id, min_block) after collecting them from the shadow walk. This sorting is critical for correct ATTACH PART ordering during restore (see https://github.com/ClickHouse/ClickHouse/issues/71009 -- Replacing/Collapsing engines depend on part insertion order).

The Rust `parse_part_name()` function exists and correctly parses from the right (to handle underscores in partition IDs), but the result is never used for sorting in the create flow.

The sorting is applied in two places in Go:
1. After `MoveShadowToBackup()` -- parts are sorted before being stored in the metadata
2. Before `AttachDataParts()` -- parts are sorted again before ATTACH during restore

**Severity: MEDIUM** -- While our restore code may do its own sorting, the manifest should store parts in a deterministic, correct order. If parts are stored unsorted in the manifest and then attached in manifest order without re-sorting, Replacing/Collapsing engine tables could produce incorrect results.

**NOTE**: Go uses a simpler split (`SplitN(name, "_", 3)`) which splits from the LEFT and gets `[partition, min_block, rest]`. This is technically incorrect for partition IDs containing underscores (e.g., tuple partitions like `2024_01_15_1_50_3`). Rust's `parse_part_name()` splits from the RIGHT which is more correct. But since Go only uses this for sorting (not parsing), and partition IDs sort correctly as strings, Go's simpler approach works in practice.

---

## 7. Manifest Creation

### 7.1 Database DDL

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Database DDL source | Queried from `system.databases` via `GetDatabases()` -- gets actual engine (Atomic, Replicated, etc.) | Hardcoded: `CREATE DATABASE IF NOT EXISTS \`{db}\` ENGINE = Atomic` | **GAP** |

**GAP DETAIL**: Go queries `system.databases` to get the actual database engine and CREATE statement. Rust hardcodes the Atomic engine for all databases. This means:
- `DatabaseReplicated` databases would be recreated as `Atomic` during restore
- Custom database engines would be lost

However, looking at the Rust restore code, it handles `DatabaseReplicated` detection separately via `query_database_engine()`, so the hardcoded DDL in the manifest may not actually be used directly. But for manifest fidelity, the actual database engine should be recorded.

**Severity: MEDIUM** -- The manifest should reflect the actual database DDL for completeness and cross-tool compatibility.

### 7.2 Per-Table Metadata vs Top-Level Manifest

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Per-table metadata | `TableMetadata` saved individually in `metadata/{db}/{table}.json` with `Parts`, `Size`, `Checksums`, `Mutations` | Similar: saved in `metadata/{db}/{table}.json` | Match |
| Checksums in per-table metadata | `Checksums map[string]uint64` (part_name -> checksum) as separate field | CRC64 embedded in each `PartInfo` | Different structure but functionally equivalent |
| Size tracking | Per-disk `Size map[string]int64` | Single `total_bytes: u64` | Go is more granular |
| Top-level manifest | `BackupMetadata` with `DataSize`, `ObjectDiskSize`, `MetadataSize`, `RBACSize`, `ConfigSize`, `NamedCollectionsSize` | `BackupManifest` with `compressed_size`, `metadata_size` | **GAP** |

**GAP DETAIL**: Go's top-level manifest tracks many size fields:
- `DataSize` -- total local disk data
- `ObjectDiskSize` -- total object disk data
- `MetadataSize` -- total metadata JSON
- `RBACSize` -- RBAC backup size
- `ConfigSize` -- config files size
- `NamedCollectionsSize` -- named collections size

Rust only tracks `compressed_size` (set during upload) and `metadata_size`. The missing size fields affect the `list` command output which should show backup sizes.

**Severity: LOW** -- Size fields are informational and already noted in CLAUDE.md as a known limitation (`rbac_size and config_size in list API response are hardcoded to 0`).

### 7.3 `required_backup` / `diff_from_remote`

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Incremental base tracking | `RequiredBackup` field in `BackupMetadata` set to `diffFromRemote` name | Not stored in manifest (diff applied locally, carried parts reference base backup's key) | **GAP** |

**GAP DETAIL**: Go stores the required backup name in the manifest's `RequiredBackup` field, which is used by retention to protect incremental chain bases. Rust handles this via the `required_backups` field in manifests during retention, but I see from CLAUDE.md that "incremental chain protection" is already implemented in Phase 6.

**Severity: NONE** -- Already handled differently in Rust.

### 7.4 Tags Field

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| `tags` in manifest | Set to "regular" for local backup, "embedded" for embedded | Not present | **MINOR GAP** |

### 7.5 Backup Version String

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Version parameter | `backupVersion` parameter passed through and stored in manifest as `ClickhouseBackupVersion` | `chbackup_version` from `CARGO_PKG_VERSION` | Different field names |

---

## 8. Error Cleanup

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| On create failure | `RemoveBackupLocal()` removes backup from ALL disks, then `Clean()` removes shadow dirs | `remove_dir_all(&backup_dir)` + `clean_shadow(ch, data_path, Some(backup_name))` | **Partial gap** |
| Multi-disk cleanup | Removes from all disk paths | Only removes from `backup_dir` (single path) | **GAP** |
| Local retention on success | `RemoveOldBackupsLocal()` called after successful create | Not called in create (handled separately) | Different design |

**GAP DETAIL**: Go's `RemoveBackupLocal()` iterates ALL disks and removes the backup directory from each disk's `backup/` path. This is important for multi-disk setups where backup shadow data is distributed across disks. Rust only removes from the primary `data_path/backup/{name}` directory.

**Severity: LOW-MEDIUM** -- In multi-disk setups, failed backups could leave orphaned data on non-default disks.

---

## 9. Edge Cases

### 9.1 Empty Tables

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Empty table handling | If `AddTableToLocalBackup` returns nil parts, table still gets metadata | If FREEZE produces empty shadow, table still gets metadata via TableManifest | Match |
| `allow_empty_backups` | Checks `CalculateNonSkipTables` (counts non-skip tables, 0 = error unless allowed) | Checks `filtered_tables.is_empty()` (0 tables = error unless allowed) | Match |

### 9.2 System Tables

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| System database filtering | Queries skip MySQL/PostgreSQL/MaterializedPostgreSQL engine databases + skip_tables pattern | Hardcoded SYSTEM_DATABASES: system, INFORMATION_SCHEMA, information_schema | **GAP** |

**GAP DETAIL**: Go dynamically queries `system.databases WHERE engine IN ('MySQL','PostgreSQL','MaterializedPostgreSQL')` to skip entire databases with external engine types. Rust only hardcodes the three standard system databases.

**Severity: LOW** -- MySQL/PostgreSQL engine databases are rare and cannot be FROZEN anyway.

### 9.3 `.inner.` / `.inner_id.` Tables (MaterializedView Inner Tables)

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Inner table enrichment | `enrichTablesByInnerDependencies()` adds `.inner.`/`.inner_id.` tables to backup if they are missing from the table list (needed for MaterializedViews) | Not implemented -- relies on `system.tables` listing all tables | **GAP** |

**GAP DETAIL**: Go has specific logic to detect MaterializedViews with inner storage tables (`.inner.{name}` or `.inner_id.{uuid}`). If these inner tables are not in the initial table list (due to pattern filtering), Go adds them automatically. Without this, restoring a MaterializedView backup might fail because the inner storage table data is missing.

**Severity: MEDIUM** -- MaterializedViews with inner storage are common. If the table pattern includes `default.*`, the inner tables would be captured. But if the pattern is more specific (e.g., `default.my_mat_view`), the inner table would be missed.

### 9.4 Distributed Tables

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Distributed engine handling | Metadata-only (no FREEZE) because it's not a MergeTree engine | `is_metadata_only_engine("Distributed") = true` | Match |

### 9.5 Resume Support During Create

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Resume on create | Checks if `metadata.json` already exists; if `--resume`, overwrites and resumes object disk uploads | Not implemented for create (resume only for upload/download/restore) | **GAP** |

**Severity: LOW** -- Create is typically fast (FREEZE + hardlink). Resume is more important for upload/download.

### 9.6 Backup Name Sanitization

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| Name cleaning | `utils.CleanBackupNameRE.ReplaceAllString(backupName, "")` | Not sanitized in create (done in freeze_name via `sanitize_name()`) | Partial match |

### 9.7 Object Disk Config Validation

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| `ValidateObjectDiskConfig` | Validates S3/GCS/Azure config before processing object disk tables | Not explicitly validated before create | **MINOR GAP** |

### 9.8 Shard Backup Type

| Aspect | Go | Rust | Gap? |
|--------|-----|------|------|
| `ShardBackupType` | Tables have `BackupType` field: `full`, `schema-only`, `none` | Not implemented -- all non-metadata-only tables are `full` | **GAP** |

**GAP DETAIL**: Go supports sharded operation mode where some tables are backed up as schema-only on certain shards (controlled by `ShardedOperationMode` config). This allows distributed backup across a cluster where each shard only backs up its own data. Rust always backs up all tables fully.

**Severity: LOW** -- Sharded operation is an advanced feature. chbackup is designed to run on each shard independently.

---

## 10. Summary of Gaps

### High Priority (correctness impact)
None found -- both implementations produce correct backups for the common case.

### Medium Priority (functionality/completeness)
1. **Engine whitelist vs blacklist** (Section 2.4): Rust should whitelist data-capable engines (`*MergeTree`, `MaterializedMySQL`, `MaterializedPostgreSQL`) rather than blacklisting metadata-only engines, to avoid attempting FREEZE on unsupported engines.
2. **Part sorting** (Section 6): Parts should be sorted by (partition_id, min_block) in the manifest for correct ATTACH ordering during restore.
3. **Database DDL from system.databases** (Section 7.1): Record actual database engine instead of hardcoding Atomic.
4. **Inner table enrichment** (Section 9.3): Automatically include `.inner.`/`.inner_id.` tables for MaterializedViews.
5. **Multi-disk cleanup on failure** (Section 8): Clean backup data from all disks on failure, not just default.

### Low Priority (edge cases, nice-to-have)
6. **`data/database/table` shadow path** (Section 3.1): Support Ordinary database engine shadow format.
7. **Encrypted disk detection** (Section 3.3): Detect encrypted disks wrapping object storage.
8. **Chown during create** (Section 4.4): Set file ownership to ClickHouse user on backup files.
9. **Skip projections per-table scoping** (Section 5.1): Support `db.table:projection` format.
10. **CH < 24.3 skip-projections warning** (Section 5.2): Warn about projection skip compatibility.
11. **Per-disk size tracking** (Section 7.2): Track data size per disk in manifest.
12. **Dynamic system database filtering** (Section 9.2): Query databases with unsupported engines.
13. **Disk type comparison case-insensitive** (Section 4 of Go `shouldSkipByDiskNameOrType`): Go uses `strings.ToLower()` for `skip_disk_types` comparison; Rust uses exact match.

### Not Gaps (intentional differences)
- Embedded backup mode: Intentionally omitted per design (S3-only streaming)
- SYNC REPLICA timing: Different but both correct approaches
- Shadow name format: Random UUID (Go) vs deterministic name (Rust) -- both work
- Staging directory layout with/without disk name: Different but manifest preserves disk info
- Local retention after create: Different design (Rust does separately)
- Shard backup type: Different design philosophy
