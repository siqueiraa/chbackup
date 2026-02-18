# Type Verification Table

All types verified against actual source code via file reads and grep.

## Existing Types Used by This Plan

| Variable/Field | Assumed Type | Actual Type | Verification Location |
|---|---|---|---|
| `config.retention.backups_to_keep_local` | `i32` | `i32` | config.rs:381 |
| `config.retention.backups_to_keep_remote` | `i32` | `i32` | config.rs:385 |
| `config.general.backups_to_keep_local` | `i32` | `i32` | config.rs:51 |
| `config.general.backups_to_keep_remote` | `i32` | `i32` | config.rs:55 |
| `config.clickhouse.data_path` | `String` | `String` | config.rs:112 |
| `BackupSummary.name` | `String` | `String` | list.rs:27 |
| `BackupSummary.timestamp` | `Option<DateTime<Utc>>` | `Option<DateTime<Utc>>` | list.rs:29 |
| `BackupSummary.is_broken` | `bool` | `bool` | list.rs:37 |
| `BackupSummary.size` | `u64` | `u64` | list.rs:31 |
| `BackupSummary.compressed_size` | `u64` | `u64` | list.rs:33 |
| `BackupSummary.table_count` | `usize` | `usize` | list.rs:35 |
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | manifest.rs:65 |
| `TableManifest.parts` | `HashMap<String, Vec<PartInfo>>` | `HashMap<String, Vec<PartInfo>>` | manifest.rs:104 |
| `PartInfo.backup_key` | `String` | `String` | manifest.rs:131 |
| `PartInfo.s3_objects` | `Option<Vec<S3ObjectInfo>>` | `Option<Vec<S3ObjectInfo>>` | manifest.rs:145 |
| `S3ObjectInfo.backup_key` | `String` | `String` | manifest.rs:159 |
| `S3Object.key` | `String` | `String` | (S3Object struct in s3.rs) |
| `S3Object.size` | `i64` | `i64` | (S3Object struct in s3.rs) |
| `DiskRow.path` | `String` | `String` | client.rs (DiskRow struct) |
| `DiskRow.name` | `String` | `String` | client.rs (DiskRow struct) |
| `RetentionConfig` | struct with 2 fields | `{ backups_to_keep_local: i32, backups_to_keep_remote: i32 }` | config.rs:377-386 |
| `Location` (list.rs) | `enum { Local, Remote }` | `enum { Local, Remote }` | list.rs:17-21 |
| `ChBackupError` | `thiserror enum` | Has variants: ClickHouseError, S3Error, ConfigError, LockError, BackupError, RestoreError, ManifestError, IoError | error.rs:1-29 |
| `LockScope` | enum with 3 variants | `{ Backup(String), Global, None }` | lock.rs:100-108 |

## Type Interaction Notes

1. **Retention config priority**: The design mentions two places for retention config -- `general.backups_to_keep_local/remote` and `retention.backups_to_keep_local/remote`. The `RetentionConfig` section (config.rs:377-386) defaults to 0 for both. The `general` section has defaults of 0 (local) and 7 (remote). Plan must decide which takes precedence -- per design, `retention.*` overrides `general.*` when non-zero.

2. **Timestamp for sorting**: `BackupSummary.timestamp` is `Option<DateTime<Utc>>`. Broken backups have `None` timestamp. For retention, broken backups are excluded from counting (per design 8.4) but should not be deleted by retention (that is clean_broken's job).

3. **i32 semantics for backups_to_keep**: 0 = unlimited (no deletion), -1 = delete local after upload (special for local only), positive = keep that many.

4. **backup_key format**: Part backup_key is a relative key like `"backup_name/data/db/table/disk/part.tar.lz4"`. For GC, we need to collect all referenced keys across all surviving manifests.

5. **S3ObjectInfo.backup_key**: For S3 disk parts, each object has its own backup_key in S3, in addition to the part-level backup_key.

## New Types This Plan Will Create

| Type | Location | Fields | Purpose |
|---|---|---|---|
| (none) | - | - | This plan adds functions, not new types |

## Functions This Plan Will Create

| Function | Location | Signature | Purpose |
|---|---|---|---|
| `retention_local` | `src/list.rs` | `fn(data_path: &str, keep: i32) -> Result<usize>` | Delete oldest local backups exceeding count |
| `retention_remote` | `src/list.rs` | `async fn(s3: &S3Client, keep: i32) -> Result<usize>` | Delete oldest remote backups (GC-safe) |
| `gc_collect_referenced_keys` | `src/list.rs` | `async fn(s3: &S3Client, exclude: &str) -> Result<HashSet<String>>` | Build referenced key set from all manifests except the one being deleted |
| `gc_delete_backup` | `src/list.rs` | `async fn(s3: &S3Client, backup_name: &str, referenced_keys: &HashSet<String>) -> Result<()>` | GC-safe delete: only remove unreferenced keys |
| `clean_shadow` | `src/list.rs` (or new file) | `async fn(ch: &ChClient, data_path: &str, name: Option<&str>) -> Result<usize>` | Walk shadow dirs, remove chbackup_* leftovers |
| `routes::clean` | `src/server/routes.rs` | Handler replacing `clean_stub` | API endpoint for /api/v1/clean |
