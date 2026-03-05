# Backup Guide

This guide covers how to create, upload, and manage backups with chbackup. It explains full and incremental backups, compression, S3 disk support, and retention.

## Table of contents

- [How backup works](#how-backup-works)
- [Creating a backup](#creating-a-backup)
- [Uploading to S3](#uploading-to-s3)
- [One-step backup (create_remote)](#one-step-backup-create_remote)
- [Incremental backups](#incremental-backups)
- [Table filtering](#table-filtering)
- [Partition filtering](#partition-filtering)
- [Compression](#compression)
- [RBAC, configs, and named collections](#rbac-configs-and-named-collections)
- [S3 disk tables](#s3-disk-tables)
- [Resumable uploads](#resumable-uploads)
- [Retention](#retention)
- [Listing backups](#listing-backups)
- [Deleting backups](#deleting-backups)
- [Cleaning up](#cleaning-up)
- [Troubleshooting](#troubleshooting)

## How backup works

A backup goes through these steps:

1. **FREEZE**: ClickHouse creates a consistent snapshot of table data using hardlinks in `shadow/`
2. **Walk**: chbackup scans the shadow directory to discover all data parts
3. **Hardlink**: Each part is hardlinked from `shadow/` to the backup directory (zero-copy, instant)
4. **CRC64**: A checksum is computed for each part for later verification
5. **UNFREEZE**: The shadow directory is cleaned up
6. **Manifest**: A `metadata.json` file is written listing all tables and parts

The backup is stored locally at `{data_path}/backup/{backup_name}/`.

## Creating a backup

```bash
# Auto-generated timestamped name
chbackup create

# Custom name
chbackup create my-backup-2024-01-15

# Backup specific tables
chbackup create -t "default.*" my-backup
```

A backup name can contain letters, numbers, hyphens, and underscores. If omitted, a timestamped name is generated.

### What is included

By default, a backup includes:

- DDL (CREATE TABLE statements) for all user tables
- All data parts for those tables
- Pending mutations (from `system.mutations`)

Excluded by default:

- System tables (`system.*`, `INFORMATION_SCHEMA.*`)
- RBAC objects (add `--rbac`)
- Server configs (add `--configs`)
- Named Collections (add `--named-collections`)

## Uploading to S3

Upload a local backup to S3:

```bash
# Upload the latest local backup
chbackup upload

# Upload a specific backup
chbackup upload my-backup

# Upload and delete the local copy
chbackup upload --delete-local my-backup
```

Each data part is independently compressed and uploaded as a separate S3 object. This means:

- Uploads are parallel (configurable concurrency)
- Individual parts can be retried on failure
- Uploads are resumable if interrupted

### S3 layout

Backups are stored under the configured prefix:

```
s3://my-bucket/chbackup/my-backup/metadata.json
s3://my-bucket/chbackup/my-backup/shadow/default/users/all_0_0_0/part.tar.lz4
s3://my-bucket/chbackup/my-backup/shadow/default/users/all_1_1_0/part.tar.lz4
...
```

## One-step backup (create_remote)

Create a local backup and upload it to S3 in one command:

```bash
chbackup create_remote

# With options
chbackup create_remote -t "default.*" --delete-source my-backup
```

This is equivalent to `create` + `upload`, but runs them in sequence and can delete the local copy afterward.

## Incremental backups

Incremental backups only store parts that changed since a base backup, significantly reducing storage and upload time.

### Local incremental

```bash
# Create a full backup
chbackup create full-2024-01

# Create an incremental backup referencing the local full backup
chbackup create --diff-from=full-2024-01 incr-2024-01-15
```

The incremental backup's manifest marks unchanged parts as "carried" from the base. Only new or modified parts are stored.

### Remote incremental

You can reference a remote backup as the incremental base in two ways:

**During create** (recommended for CronJob workflows): downloads the remote manifest from S3 and skips hardlinks for parts that match by CRC64, saving local disk I/O and space:

```bash
chbackup create --diff-from-remote=full-remote incr-backup
```

**During upload**: skips uploading parts that already exist in the remote base:

```bash
chbackup upload --diff-from-remote=full-remote incr-backup
```

Both approaches produce the same result — parts that match the remote base are marked as "carried" in the manifest.

### With create_remote

```bash
chbackup create_remote --diff-from-remote=full-2024-01 incr-2024-01-15
```

### Incremental chain protection

The retention system protects incremental base backups from deletion. If backup B references backup A as its diff-from base, A will not be deleted by retention even if it is older than the keep count.

### Restoring incremental backups

Restoring an incremental backup works identically to restoring a full backup. chbackup resolves the "carried" references and downloads parts from the correct source.

## Table filtering

Use glob patterns to select which tables to back up:

```bash
# All tables in a database
chbackup create -t "default.*"

# A specific table
chbackup create -t "default.users"

# Multiple patterns (comma-separated)
chbackup create -t "default.users,default.orders"

# All tables with a name pattern
chbackup create -t "*.events"
```

### Default exclusions

These tables are excluded by default (configurable via `clickhouse.skip_tables`):

- `system.*`
- `INFORMATION_SCHEMA.*`
- `information_schema.*`

### Skip table engines

Skip tables by engine type:

```yaml
clickhouse:
  skip_table_engines: ["Kafka", "S3Queue", "RabbitMQ"]
```

## Partition filtering

Back up only specific partitions:

```bash
chbackup create --partitions="202401,202402" my-backup
```

This triggers per-partition FREEZE instead of whole-table FREEZE. Only the named partitions are included in the backup.

For tables without a PARTITION BY clause, all data is in a single partition named `all`.

## Compression

Control compression algorithm and level:

```yaml
backup:
  compression: lz4       # lz4, zstd, gzip, none
  compression_level: 1   # algorithm-dependent
```

| Algorithm | Speed | Ratio | Notes |
|-----------|-------|-------|-------|
| `lz4` | Fast | ~2x | Default. Best for most workloads |
| `zstd` | Medium | ~3-4x | Better ratio, higher CPU. Level 1-22 |
| `gzip` | Slow | ~3x | Wide compatibility. Level 1-9 |
| `none` | Instant | 1x | No compression. For testing or pre-compressed data |

### Streaming upload for large parts

Parts larger than 256 MiB (configurable) use streaming multipart upload. Instead of buffering the entire compressed part in memory, chunks are uploaded as they are produced:

```yaml
backup:
  streaming_upload_threshold: 268435456  # 256 MiB in bytes
```

This keeps memory usage bounded regardless of part size.

## RBAC, configs, and named collections

### RBAC objects

Back up users, roles, quotas, row policies, and settings profiles:

```bash
chbackup create --rbac my-backup
```

To always include RBAC without the flag:

```yaml
clickhouse:
  rbac_backup_always: true
```

### ClickHouse configuration

Back up server config files from `config_dir`:

```bash
chbackup create --configs my-backup
```

### Named Collections

```bash
chbackup create --named-collections my-backup
```

### All together

```bash
chbackup create --rbac --configs --named-collections full-backup
```

## S3 disk tables

Tables stored on ClickHouse S3 disks (object_storage type) are handled differently:

- Data is not hardlinked locally (it lives in S3)
- During backup, only metadata files are collected
- During upload, S3 CopyObject is used (server-side copy, no data transfer through chbackup)
- During restore, CopyObject copies objects to the new table's UUID path

This happens automatically. No special flags are needed.

### Disk filtering

Skip specific disks or disk types:

```yaml
clickhouse:
  skip_disks: ["cold_storage"]
  skip_disk_types: ["cache"]
```

## Resumable uploads

If an upload is interrupted (network failure, crash, Ctrl+C), resume from where it left off:

```bash
chbackup upload --resume my-backup
```

Resume state is saved after each part is uploaded. On resume, already-uploaded parts are skipped. The state file is stored at `{data_path}/backup/{backup_name}/.upload_state`.

Resume validation: if you change upload parameters (e.g., different `--diff-from-remote`), the state file is invalidated and the upload starts from scratch.

To disable resume state tracking:

```yaml
general:
  use_resumable_state: false
```

## Retention

chbackup can automatically delete old backups after upload.

### Local retention

```yaml
general:
  backups_to_keep_local: 3   # keep last 3 local backups
```

Special values:

- `0` -- unlimited (no cleanup)
- `-1` -- delete local backup immediately after successful upload

### Remote retention

```yaml
general:
  backups_to_keep_remote: 7   # keep last 7 remote backups
```

`0` means unlimited.

### How remote retention works

After each upload:

1. List all remote backups (excluding broken ones)
2. Sort by creation time (oldest first)
3. For each backup exceeding the keep count:
   - Collect all S3 keys referenced by surviving backups
   - Delete only unreferenced keys (manifest deleted last)

This garbage-collection approach ensures that shared parts (from incremental backups) are not deleted while any backup still references them.

### Override retention per-section

The `retention` config section overrides `general.*` when non-zero:

```yaml
general:
  backups_to_keep_remote: 7     # default
retention:
  backups_to_keep_remote: 14    # overrides general
```

## Listing backups

```bash
# List all backups (local + remote)
chbackup list

# Local only
chbackup list local

# Remote only
chbackup list remote

# JSON output
chbackup list --format=json

# Other formats: yaml, csv, tsv
chbackup list --format=csv
```

### Shortcut aliases

`latest` and `previous` resolve to the most recent and second-most-recent backup names:

```bash
chbackup upload latest
chbackup restore latest
chbackup restore_remote previous
```

## Deleting backups

```bash
# Delete a local backup
chbackup delete local my-backup

# Delete a remote backup (from S3)
chbackup delete remote my-backup
```

Remote deletion uses the same garbage-collection approach as retention: only S3 keys not referenced by other backups are removed.

## Cleaning up

### Clean shadow directories

If a backup is interrupted before UNFREEZE, leftover shadow directories remain on disk:

```bash
chbackup clean
chbackup clean --name my-failed-backup
```

### Clean broken backups

Broken backups have missing or corrupt `metadata.json`:

```bash
chbackup clean_broken local
chbackup clean_broken remote
```

## Troubleshooting

### "No tables matched the filter"

The table filter matched no tables. Check:

1. The pattern syntax: use `database.table` format with glob wildcards (`*`)
2. Default exclusions: system tables are excluded by default
3. Run `chbackup tables -t "your_pattern"` to see what matches

If you intentionally want an empty backup:

```yaml
backup:
  allow_empty_backups: true
```

### Backup is slow

- **Many small tables**: Bottleneck is per-table FREEZE. Consider `clickhouse.freeze_by_part: false` (the default) to freeze whole tables at once.
- **Large parts**: Check `general.upload_concurrency` (default: 4). Increase for more parallelism.
- **S3 disk tables**: CopyObject is limited by `general.object_disk_server_side_copy_concurrency` (default: 32).
- **Rate limiting**: Check if `general.upload_max_bytes_per_second` is set.

### "Lock conflict" (exit code 4)

Another chbackup process is running. Only one backup operation can run at a time (per data_path). Wait for it to finish or check for stale PID locks.

### "Column type mismatch" warning

ClickHouse parts can have slightly different column types after ALTER TABLE. By default, chbackup warns but continues. To enforce consistency:

```yaml
clickhouse:
  check_parts_columns: true
```

To suppress the check entirely:

```bash
chbackup create --skip-check-parts-columns my-backup
```

### Upload fails partway through

Use `--resume` to pick up where it left off:

```bash
chbackup upload --resume my-backup
```

If resume does not work (e.g., you changed parameters), delete the state file and retry:

```bash
rm {data_path}/backup/my-backup/.upload_state
chbackup upload my-backup
```
