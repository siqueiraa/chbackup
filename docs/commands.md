# CLI Command Reference

All commands accept a global `-c` / `--config` flag to specify the config file path. When not provided, chbackup checks `CHBACKUP_CONFIG`, then `CLICKHOUSE_BACKUP_CONFIG` (Go compat), then `/etc/chbackup/config.yml`, then `/etc/clickhouse-backup/config.yml` (Go compat). See [Configuration > Config file location](configuration.md#config-file-location) for the full fallback chain.

```bash
chbackup -c /path/to/config.yml <command> [options]
```

You can also override individual config values with `--env`:

```bash
chbackup --env s3.bucket=other-bucket create my-backup
```

## Table of contents

- [create](#create)
- [upload](#upload)
- [download](#download)
- [restore](#restore)
- [create_remote](#create_remote)
- [restore_remote](#restore_remote)
- [list](#list)
- [tables](#tables)
- [delete](#delete)
- [clean](#clean)
- [clean_broken](#clean_broken)
- [watch](#watch)
- [server](#server)
- [default-config](#default-config)
- [print-config](#print-config)

---

## create

Create a local backup by freezing ClickHouse tables and hardlinking data parts to the backup directory.

```bash
chbackup create [OPTIONS] [BACKUP_NAME]
```

If `BACKUP_NAME` is omitted, a timestamped name like `2024-01-15T12-00-00` is generated.

### Flags

| Flag | Description |
|------|-------------|
| `-t, --tables PATTERN` | Table filter (glob patterns: `default.*`, `*.users`, `db.table`) |
| `--partitions LIST` | Only backup specific partitions (comma-separated) |
| `--diff-from NAME` | Create an incremental backup based on a local backup |
| `--skip-projections PATTERNS` | Glob patterns for projections to skip (comma-separated, `*` = all) |
| `--schema` | Backup schema (DDL) only, no data |
| `--rbac` | Include RBAC objects (users, roles, quotas, row policies, settings profiles) |
| `--configs` | Include ClickHouse server configuration files |
| `--named-collections` | Include Named Collections |
| `--skip-check-parts-columns` | Allow backup even if column types are inconsistent across parts |

### Examples

```bash
# Backup all tables
chbackup create

# Backup with a custom name
chbackup create prod-backup-2024-01-15

# Backup only the default database
chbackup create -t "default.*"

# Backup specific tables
chbackup create -t "default.users,default.orders"

# Incremental backup (only changed parts since base)
chbackup create --diff-from=full-backup incr-backup

# Backup only specific partitions (e.g., January and February 2024)
chbackup create --partitions="202401,202402"

# Schema-only backup (no data)
chbackup create --schema schema-backup

# Include everything (RBAC, configs, named collections)
chbackup create --rbac --configs --named-collections full-backup

# Skip all projections
chbackup create --skip-projections="*"
```

---

## upload

Upload a local backup to S3.

```bash
chbackup upload [OPTIONS] [BACKUP_NAME]
```

If `BACKUP_NAME` is omitted, the most recent local backup is used.

### Flags

| Flag | Description |
|------|-------------|
| `--delete-local` | Delete the local backup after successful upload |
| `--diff-from-remote NAME` | Upload incrementally, referencing a remote backup as base |
| `--resume` | Resume an interrupted upload from the saved state file |

### Examples

```bash
# Upload the latest backup
chbackup upload

# Upload a specific backup
chbackup upload prod-backup-2024-01-15

# Upload and delete the local copy
chbackup upload --delete-local my-backup

# Incremental upload (only parts not in the remote base)
chbackup upload --diff-from-remote=full-remote incr-backup

# Resume a failed upload
chbackup upload --resume my-backup
```

---

## download

Download a backup from S3 to local storage.

```bash
chbackup download [OPTIONS] [BACKUP_NAME]
```

### Flags

| Flag | Description |
|------|-------------|
| `--hardlink-exists-files` | Deduplicate parts by hardlinking to matching parts in other local backups |
| `--resume` | Resume an interrupted download from the saved state file |

### Examples

```bash
# Download the latest remote backup
chbackup download latest

# Download a specific backup
chbackup download prod-backup-2024-01-15

# Download with deduplication (saves disk space if you have other local backups)
chbackup download --hardlink-exists-files my-backup

# Resume a failed download
chbackup download --resume my-backup
```

---

## restore

Restore a local backup into ClickHouse.

```bash
chbackup restore [OPTIONS] [BACKUP_NAME]
```

### Flags

| Flag | Description |
|------|-------------|
| `-t, --tables PATTERN` | Restore only matching tables (glob pattern) |
| `--as MAPPING` | Rename tables during restore (see examples below) |
| `-m, --database-mapping MAP` | Remap databases (e.g., `prod:staging`) |
| `--partitions LIST` | Restore only specific partitions (comma-separated) |
| `--schema` | Restore schema (DDL) only, no data |
| `--data-only` | Restore data only, skip DDL (tables must already exist) |
| `--rm, --drop` | DROP existing tables before restore (Mode A, destructive) |
| `--resume` | Resume an interrupted restore from the saved state file |
| `--rbac` | Restore RBAC objects |
| `--configs` | Restore ClickHouse server configuration files |
| `--named-collections` | Restore Named Collections |
| `--skip-empty-tables` | Skip CREATE for tables that have no matching data parts |

### Examples

```bash
# Restore all tables (non-destructive, adds data to existing tables)
chbackup restore my-backup

# Clean restore (drop existing tables first)
chbackup restore --rm my-backup

# Restore only specific tables
chbackup restore -t "default.users" my-backup

# Rename a single table during restore
chbackup restore -t default.users --as=default.users_restored my-backup

# Rename multiple tables
chbackup restore --as="default.users:default.users_copy,default.orders:default.orders_copy" my-backup

# Remap an entire database
chbackup restore -m "production:staging" my-backup

# Remap multiple databases
chbackup restore -m "production:staging,analytics:analytics_test" my-backup

# Restore only January 2024 partition
chbackup restore --partitions="202401" my-backup

# Schema only (create tables without data)
chbackup restore --schema my-backup

# Data only (tables must already exist)
chbackup restore --data-only my-backup

# Restore RBAC and configs
chbackup restore --rbac --configs my-backup

# Resume a failed restore
chbackup restore --resume my-backup
```

### Restore modes

**Mode B (default)**: Non-destructive. Creates databases and tables if they do not exist, then attaches data parts. Existing data is preserved. Safe to run multiple times.

**Mode A (--rm)**: Destructive. Drops existing tables and databases before restoring. Use when you want a clean restore that exactly matches the backup state.

---

## create_remote

Create a local backup and upload it to S3 in one step. Equivalent to running `create` followed by `upload`.

```bash
chbackup create_remote [OPTIONS] [BACKUP_NAME]
```

### Flags

| Flag | Description |
|------|-------------|
| `-t, --tables PATTERN` | Table filter pattern |
| `--diff-from-remote NAME` | Remote incremental base backup |
| `--delete-source` | Delete local backup after successful upload |
| `--rbac` | Include RBAC objects |
| `--configs` | Include ClickHouse server configuration files |
| `--named-collections` | Include Named Collections |
| `--skip-check-parts-columns` | Allow inconsistent column types |
| `--skip-projections PATTERNS` | Projections to skip |
| `--resume` | Resume interrupted operation |

### Examples

```bash
# Full backup in one step
chbackup create_remote

# Incremental backup
chbackup create_remote --diff-from-remote=base-backup incr-backup

# Backup specific tables and clean up local copy
chbackup create_remote -t "default.*" --delete-source
```

---

## restore_remote

Download a remote backup from S3 and restore it in one step. Equivalent to `download` followed by `restore`.

```bash
chbackup restore_remote [OPTIONS] [BACKUP_NAME]
```

### Flags

| Flag | Description |
|------|-------------|
| `-t, --tables PATTERN` | Table filter pattern |
| `--as MAPPING` | Rename tables during restore |
| `-m, --database-mapping MAP` | Remap databases |
| `--rm, --drop` | DROP existing tables before restore |
| `--rbac` | Restore RBAC objects |
| `--configs` | Restore ClickHouse server configuration files |
| `--named-collections` | Restore Named Collections |
| `--skip-empty-tables` | Skip tables with no matching data parts |
| `--resume` | Resume interrupted operation |

### Examples

```bash
# Download and restore the latest backup
chbackup restore_remote latest

# Clean restore from remote
chbackup restore_remote --rm my-backup

# Restore specific tables to a different database
chbackup restore_remote -t "default.*" -m "default:staging" my-backup
```

---

## list

List backups.

```bash
chbackup list [local|remote] [--format FORMAT]
```

If no location is given, shows both local and remote backups.

### Flags

| Flag | Description |
|------|-------------|
| `--format FORMAT` | Output format: `default` (table), `json`, `yaml`, `csv`, `tsv` |

### Examples

```bash
# List all backups
chbackup list

# List only local backups
chbackup list local

# List only remote backups
chbackup list remote

# JSON output (useful for scripting)
chbackup list --format=json

# CSV output
chbackup list --format=csv
```

### Shortcut aliases

Use `latest` and `previous` as backup name aliases in other commands:

```bash
chbackup upload latest
chbackup restore_remote previous
```

---

## tables

List tables from ClickHouse or from a remote backup manifest.

```bash
chbackup tables [OPTIONS]
```

### Flags

| Flag | Description |
|------|-------------|
| `-t, --tables PATTERN` | Filter by glob pattern |
| `--all` | Include system tables |
| `--remote-backup NAME` | Show tables from a remote backup instead of live ClickHouse |

### Examples

```bash
# List all user tables
chbackup tables

# List all tables including system
chbackup tables --all

# Filter to a specific database
chbackup tables -t "default.*"

# See what tables are in a remote backup
chbackup tables --remote-backup=my-backup
```

---

## delete

Delete a backup.

```bash
chbackup delete <local|remote> [BACKUP_NAME]
```

### Examples

```bash
# Delete a local backup
chbackup delete local my-backup

# Delete a remote backup (removes from S3)
chbackup delete remote my-backup
```

Remote deletion uses garbage collection: only S3 keys that are not referenced by other backups are deleted. This is safe to run even with incremental backup chains.

---

## clean

Remove leftover `shadow/` directories from previous FREEZE operations. These are normally cleaned up automatically, but can be left behind if a backup is interrupted.

```bash
chbackup clean [--name BACKUP_NAME]
```

### Examples

```bash
# Clean all chbackup shadow directories
chbackup clean

# Clean shadow directories for a specific backup
chbackup clean --name my-failed-backup
```

---

## clean_broken

Remove broken backups that have missing or corrupt `metadata.json`.

```bash
chbackup clean_broken <local|remote>
```

### Examples

```bash
# Clean broken local backups
chbackup clean_broken local

# Clean broken remote backups
chbackup clean_broken remote
```

---

## watch

Run a scheduled backup loop. Alternates between full and incremental backups on a configurable schedule.

```bash
chbackup watch [OPTIONS]
```

### Flags

| Flag | Description |
|------|-------------|
| `--watch-interval DURATION` | Time between backup checks (e.g., `1h`, `30m`) |
| `--full-interval DURATION` | Time between full backups (e.g., `24h`) |
| `--name-template TEMPLATE` | Backup name template with macros |
| `-t, --tables PATTERN` | Table filter pattern |

### Examples

```bash
# Run with default config
chbackup watch

# Custom intervals
chbackup watch --watch-interval=30m --full-interval=12h

# Custom name template
chbackup watch --name-template="shard{shard}-{type}-{time:%Y%m%d_%H%M%S}"

# Watch only specific tables
chbackup watch -t "default.*"
```

### Name template macros

| Macro | Description | Example |
|-------|-------------|---------|
| `{type}` | Backup type | `full` or `incremental` |
| `{time:FORMAT}` | Timestamp (chrono format) | `{time:%Y%m%d_%H%M%S}` -> `20240115_120000` |
| `{shard}` | Shard name from ClickHouse `system.macros` | `01` |

The watch loop resumes after restart by scanning remote backups matching the template prefix.

---

## server

Start the HTTP API server. Designed for Kubernetes sidecar deployments.

```bash
chbackup server [OPTIONS]
```

### Flags

| Flag | Description |
|------|-------------|
| `--watch` | Run the watch loop alongside the API server |
| `--watch-interval DURATION` | Override the watch interval |
| `--full-interval DURATION` | Override the full backup interval |

### Examples

```bash
# API server only
chbackup server

# API server with scheduled backups
chbackup server --watch

# With custom intervals
chbackup server --watch --watch-interval=2h --full-interval=48h
```

See the [API documentation](api.md) for the full endpoint reference.

---

## default-config

Print the default configuration to stdout. Useful as a starting point for creating your config file.

```bash
chbackup default-config > /etc/chbackup/config.yml
```

---

## print-config

Print the fully resolved configuration after applying the config file, environment variables, and CLI overrides. Useful for debugging configuration issues.

```bash
chbackup print-config
chbackup -c /path/to/config.yml print-config
```
