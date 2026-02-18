# Data Authority Analysis

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| List of local backups | `list_local()` | `Vec<BackupSummary>` with name, timestamp, is_broken | USE EXISTING |
| List of remote backups | `list_remote()` | `Vec<BackupSummary>` with name, timestamp, is_broken | USE EXISTING |
| Backup timestamp for sorting | `BackupSummary.timestamp` | `Option<DateTime<Utc>>` | USE EXISTING |
| Broken backup flag | `BackupSummary.is_broken` | `bool` | USE EXISTING |
| All S3 keys for a backup | `S3Client::list_objects(prefix)` | `Vec<S3Object>` with key, size | USE EXISTING |
| Referenced keys in manifest | `BackupManifest.tables[*].parts[*][*].backup_key` | `String` | USE EXISTING |
| Referenced S3 disk keys | `PartInfo.s3_objects[*].backup_key` | `String` | USE EXISTING |
| Manifest JSON from S3 | `S3Client::get_object(key)` | `Vec<u8>` -> `BackupManifest::from_json_bytes()` | USE EXISTING |
| Disk paths for shadow walk | `ChClient::get_disks()` | `Vec<DiskRow>` with name, path, type_field | USE EXISTING |
| Retention config | `Config.retention` | `RetentionConfig { backups_to_keep_local, backups_to_keep_remote }` | USE EXISTING |
| General retention config | `Config.general` | `backups_to_keep_local: i32, backups_to_keep_remote: i32` | USE EXISTING |
| Delete local backup | `list::delete_local()` | `fn(data_path, name) -> Result<()>` | USE EXISTING |
| Delete remote backup | `list::delete_remote()` | `async fn(s3, name) -> Result<()>` | USE EXISTING -- but for GC we need a modified version |
| Batch delete S3 keys | `S3Client::delete_objects()` | `async fn(keys: Vec<String>) -> Result<()>` | USE EXISTING |
| S3 prefix for key stripping | `S3Client::prefix()` | `&str` | USE EXISTING |

## Analysis Notes

1. **No new data sources needed.** All data required for retention/GC is already queryable via existing `list_*`, `S3Client`, and `BackupManifest` APIs.

2. **GC referenced key collection is the only new computation.** We must load all surviving manifests and extract all `backup_key` values from all parts (including S3 object keys). This is new logic but reads from existing data structures.

3. **Shadow directory cleanup (clean command)** uses `ChClient::get_disks()` for disk paths and `std::fs::read_dir` for walking shadow directories. No new data source needed.

4. **Retention config resolution**: Both `retention.*` and `general.*` have retention fields. The design doc (8.3) uses `retention:` section. The plan should resolve: use `retention.*` when non-zero, fall back to `general.*`. This is a new helper function but NOT new data.

5. **Manifest caching (server mode)**: Design 8.2 mentions caching manifest key-sets. For the initial implementation, we will NOT cache -- we will load all manifests each time. Caching is an optimization that can be added when the number of remote backups makes per-GC manifest loading too slow. Document this as a known limitation / future optimization.

## Must Implement (with justification)

| Component | Justification |
|-----------|---------------|
| `retention_local()` | No existing retention function; clean_broken is the closest but deletes by broken status, not by count |
| `retention_remote()` with GC | No existing function; must implement the GC algorithm from design 8.2 |
| `gc_collect_referenced_keys()` | New computation: union of all backup_key values across all surviving manifests |
| `clean_shadow()` | No existing shadow cleanup; design 13 specifies walking shadow dirs |
| Effective retention config resolution | No existing helper to merge retention.* with general.* |
