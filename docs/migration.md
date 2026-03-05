# Migrating from Go clickhouse-backup

chbackup is a drop-in replacement for [Altinity/clickhouse-backup](https://github.com/Altinity/clickhouse-backup). In most cases, you can swap the binary or Docker image with minimal changes.

## Step 1: Replace the binary or image

**Docker / Kubernetes:**

```yaml
containers:
  - name: clickhouse-backup
-   image: altinity/clickhouse-backup:latest
+   image: siqueiraa/chbackup:latest
```

**Bare metal:**

Download the static binary from the [releases page](https://github.com/siqueiraa/chbackup/releases) and replace the existing `clickhouse-backup` binary.

## Step 2: Keep existing environment variables

Go clickhouse-backup environment variables are accepted as fallbacks. The chbackup-native name always takes precedence if both are set.

| Go env var | chbackup env var | Config field |
|---|---|---|
| `LOG_LEVEL` | `CHBACKUP_LOG_LEVEL` | `general.log_level` |
| `BACKUPS_TO_KEEP_LOCAL` | `CHBACKUP_BACKUPS_TO_KEEP_LOCAL` | `general.backups_to_keep_local` |
| `BACKUPS_TO_KEEP_REMOTE` | `CHBACKUP_BACKUPS_TO_KEEP_REMOTE` | `general.backups_to_keep_remote` |
| `S3_PATH` | `S3_PREFIX` | `s3.prefix` |
| `S3_UPLOAD_CONCURRENCY` | `CHBACKUP_BACKUP_UPLOAD_CONCURRENCY` | `backup.upload_concurrency` |
| `S3_DOWNLOAD_CONCURRENCY` | `CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY` | `backup.download_concurrency` |
| `CLICKHOUSE_FREEZE_BY_PART` | — | `clickhouse.freeze_by_part` |
| `ALLOW_EMPTY_BACKUPS` | — | `backup.allow_empty_backups` |
| `CLICKHOUSE_BACKUP_CONFIG` | `CHBACKUP_CONFIG` | Config file path |
| `REMOTE_STORAGE` | — | Accepted but ignored (S3-only) |

## Step 3: Port remap

Go clickhouse-backup uses the native TCP protocol (port 9000) while chbackup uses the HTTP protocol (port 8123). When `CLICKHOUSE_PORT=9000` is detected, chbackup automatically remaps it to 8123 with a warning.

Set `CLICKHOUSE_PORT=8123` explicitly to suppress the warning.

## Step 4: Config file path

chbackup auto-detects Go config paths:

1. `CLICKHOUSE_BACKUP_CONFIG` environment variable
2. `/etc/clickhouse-backup/config.yml` (if the file exists)

These are checked after the chbackup-native paths (`CHBACKUP_CONFIG`, `/etc/chbackup/config.yml`). See [Configuration > Config file location](configuration.md#config-file-location) for the full fallback chain.

## Step 5: API compatibility

Both API route prefixes work:

- `/api/v1/*` — chbackup native routes (JSON arrays)
- `/backup/*` — Go-compatible routes (JSONEachRow format)

Existing CronJobs, scripts, and ClickHouse URL engine tables (`system.backup_list`, `system.backup_actions`) pointing to `/backup/*` endpoints continue working without changes.

See [API docs > Legacy Go compatibility](api.md#legacy-go-compatibility-backup-routes) for response format details.

## Step 6: REMOTE_STORAGE

The `REMOTE_STORAGE` env var is accepted for compatibility. If set to `"s3"` or empty, it's a no-op. Any other value logs a warning since chbackup only supports S3 storage. If you were using GCS, Azure, or other backends, chbackup is not a direct replacement.

## Docker Compose example

```yaml
services:
  clickhouse-backup:
-   image: altinity/clickhouse-backup:latest
+   image: siqueiraa/chbackup:latest
    environment:
      # These Go env vars work as-is:
      S3_BUCKET: my-backups
      S3_PATH: clickhouse/  # mapped to s3.prefix
      LOG_LEVEL: info
      BACKUPS_TO_KEEP_REMOTE: "7"
      CLICKHOUSE_PORT: "8123"
```

## What is different

- **S3 only** — no GCS, Azure, SFTP, or FTP backends
- **HTTP protocol** — connects to ClickHouse via HTTP (8123), not native TCP (9000)
- **Rust binary** — ~15 MB vs ~80 MB, same functionality for S3 use cases
