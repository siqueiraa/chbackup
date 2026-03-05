# HTTP API Reference

chbackup includes an HTTP API server for managing backups programmatically. Start it with:

```bash
chbackup server
chbackup server --watch  # with scheduled backups
```

The server listens on `localhost:7171` by default. Change with `api.listen` config or `API_LISTEN` env var.

## Table of contents

- [Authentication](#authentication)
- [Concurrency](#concurrency)
- [Health and status](#health-and-status)
- [Backup operations](#backup-operations)
- [Listing and information](#listing-and-information)
- [Cleanup](#cleanup)
- [Operation control](#operation-control)
- [Watch lifecycle](#watch-lifecycle)
- [Configuration management](#configuration-management)
- [Metrics](#metrics)
- [Command dispatcher](#command-dispatcher)
- [Legacy Go compatibility (`/backup/*` routes)](#legacy-go-compatibility-backup-routes)
- [Error responses](#error-responses)

## Authentication

When `api.username` and `api.password` are set, all endpoints require HTTP Basic auth:

```bash
curl -u admin:secret http://localhost:7171/api/v1/list
```

Without auth configured, all endpoints are open.

## Concurrency

By default, only one backup operation can run at a time. A second request returns HTTP 423:

```json
{"error": "operation already running: upload my-backup"}
```

To allow parallel operations on different backup names, set `api.allow_parallel: true`.

## Health and status

### GET /health

Returns a simple health check. Use for Kubernetes liveness probes.

```bash
curl http://localhost:7171/health
```

```json
{"status": "ok"}
```

### GET /api/v1/version

Returns the server version.

```bash
curl http://localhost:7171/api/v1/version
```

### GET /api/v1/status

Returns the current operation state. Shows `idle` when nothing is running, or the command name and start time when an operation is in progress. Use for readiness probes.

```bash
curl http://localhost:7171/api/v1/status
```

Example response (idle):

```json
{"status": "idle", "command": null, "start": null}
```

Example response (operation running):

```json
{"status": "running", "command": "upload my-backup", "start": "2024-01-15T12:05:00Z"}
```

## Backup operations

### POST /api/v1/create

Create a new local backup.

```bash
# Default (all tables, auto-generated name)
curl -X POST http://localhost:7171/api/v1/create

# With options
curl -X POST http://localhost:7171/api/v1/create \
  -H "Content-Type: application/json" \
  -d '{
    "backup_name": "my-backup",
    "tables": "default.*",
    "rbac": true,
    "configs": true
  }'
```

### POST /api/v1/upload/:name

Upload a local backup to S3.

```bash
curl -X POST http://localhost:7171/api/v1/upload/my-backup

# With incremental base
curl -X POST http://localhost:7171/api/v1/upload/my-backup \
  -H "Content-Type: application/json" \
  -d '{"diff_from_remote": "base-backup"}'
```

### POST /api/v1/download/:name

Download a backup from S3.

```bash
curl -X POST http://localhost:7171/api/v1/download/my-backup
```

### POST /api/v1/restore/:name

Restore a local backup to ClickHouse.

```bash
# Basic restore
curl -X POST http://localhost:7171/api/v1/restore/my-backup

# With options (destructive mode, table filter)
curl -X POST http://localhost:7171/api/v1/restore/my-backup \
  -H "Content-Type: application/json" \
  -d '{
    "rm": true,
    "tables": "default.users"
  }'
```

### POST /api/v1/create_remote

Create and upload in one step.

```bash
curl -X POST http://localhost:7171/api/v1/create_remote

# With options
curl -X POST http://localhost:7171/api/v1/create_remote \
  -H "Content-Type: application/json" \
  -d '{
    "backup_name": "my-backup",
    "tables": "default.*",
    "diff_from_remote": "base-backup"
  }'
```

### POST /api/v1/restore_remote/:name

Download and restore in one step.

```bash
curl -X POST http://localhost:7171/api/v1/restore_remote/my-backup
```

### DELETE /api/v1/delete/:location/:name

Delete a backup. `:location` is `local` or `remote`.

```bash
# Delete local backup
curl -X DELETE http://localhost:7171/api/v1/delete/local/my-backup

# Delete remote backup
curl -X DELETE http://localhost:7171/api/v1/delete/remote/my-backup
```

## Listing and information

### GET /api/v1/list

List all backups.

```bash
# List all backups (local + remote)
curl http://localhost:7171/api/v1/list

# List only remote backups
curl "http://localhost:7171/api/v1/list?location=remote"

# Paginated, newest first
curl "http://localhost:7171/api/v1/list?offset=0&limit=10&desc=true"

# JSON format
curl "http://localhost:7171/api/v1/list?format=json"
```

Query parameters:

| Parameter | Description | Default |
|-----------|-------------|---------|
| `location` | Filter by location: `local` or `remote` | both |
| `offset` | Skip N entries | 0 |
| `limit` | Return at most N entries | unlimited |
| `desc` | Reverse sort (newest first) | `false` |
| `format` | Accepted for compatibility; API always returns JSON | JSON |

The response includes an `X-Total-Count` header with the total number of backups before pagination.

### GET /api/v1/tables

List ClickHouse tables.

```bash
# All tables from live ClickHouse
curl http://localhost:7171/api/v1/tables

# Filter by pattern
curl "http://localhost:7171/api/v1/tables?table=default.*"

# Tables from a remote backup
curl "http://localhost:7171/api/v1/tables?backup=my-backup"

# Paginated
curl "http://localhost:7171/api/v1/tables?offset=0&limit=20"
```

Query parameters:

| Parameter | Description | Default |
|-----------|-------------|---------|
| `table` | Glob filter pattern | all tables |
| `all` | Include system tables (`true`/`false`) | `false` |
| `backup` | Load from remote backup instead of live ClickHouse | live |
| `offset` | Skip N entries | 0 |
| `limit` | Return at most N entries | unlimited |

Returns `X-Total-Count` header.

### GET /api/v1/actions

Get the history of operations.

```bash
curl http://localhost:7171/api/v1/actions
```

Returns a list of recorded actions. Each entry includes `id`, `command`, `start`, `finish`, `status`, and `error` fields.

## Cleanup

### POST /api/v1/clean

Remove leftover shadow directories from FREEZE operations.

```bash
curl -X POST http://localhost:7171/api/v1/clean
```

### POST /api/v1/clean/local_broken

Delete local backups with missing or corrupt metadata.

```bash
curl -X POST http://localhost:7171/api/v1/clean/local_broken
```

### POST /api/v1/clean/remote_broken

Delete remote (S3) backups with missing or corrupt metadata.

```bash
curl -X POST http://localhost:7171/api/v1/clean/remote_broken
```

## Operation control

### POST /api/v1/kill

Cancel a running operation.

```bash
# Cancel all running operations
curl -X POST http://localhost:7171/api/v1/kill

# Cancel a specific operation by ID
curl -X POST "http://localhost:7171/api/v1/kill?id=1"
```

The operation stops after completing its current part. Resume state is saved if `general.use_resumable_state` is enabled.

## Watch lifecycle

### GET /api/v1/watch/status

Get the current state of the watch loop.

```bash
curl http://localhost:7171/api/v1/watch/status
```

Example response:

```json
{
  "active": true,
  "state": "sleeping",
  "last_full": "2024-01-15T12:00:00Z",
  "last_incr": "2024-01-15T13:00:00Z",
  "consecutive_errors": 0,
  "next_in": "45m"
}
```

### POST /api/v1/watch/start

Start the watch loop. Has no effect if already running.

```bash
# Start with default intervals from config
curl -X POST http://localhost:7171/api/v1/watch/start

# Start with custom intervals
curl -X POST http://localhost:7171/api/v1/watch/start \
  -H "Content-Type: application/json" \
  -d '{"watch_interval": "30m", "full_interval": "12h"}'
```

### POST /api/v1/watch/stop

Stop the watch loop. The current cycle completes before stopping.

```bash
curl -X POST http://localhost:7171/api/v1/watch/stop
```

## Configuration management

### POST /api/v1/reload

Reload the config file from disk. Does not verify ClickHouse connectivity.

```bash
curl -X POST http://localhost:7171/api/v1/reload
```

Use this after editing the config file or ConfigMap. The server picks up new values without restarting.

### POST /api/v1/restart

Reload the config, recreate the ClickHouse and S3 clients, and verify the ClickHouse connection. If the connection fails, the old clients remain active.

```bash
curl -X POST http://localhost:7171/api/v1/restart
```

Use this to refresh STS credentials or after changing ClickHouse connection settings.

## Metrics

### GET /metrics

Prometheus metrics endpoint. Enabled by default (disable with `api.enable_metrics: false`).

```bash
curl http://localhost:7171/metrics
```

See the [Kubernetes guide](kubernetes.md#prometheus-monitoring) for scraping configuration.

## Command dispatcher

### POST /api/v1/actions

A generic command dispatcher. The body is a JSON object with a `command` field. The command is a single string with the operation and backup name separated by spaces.

```bash
# Create a backup
curl -X POST http://localhost:7171/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{"command": "create my-backup"}'

# Upload a backup
curl -X POST http://localhost:7171/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{"command": "upload my-backup"}'

# Delete a remote backup
curl -X POST http://localhost:7171/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{"command": "delete remote my-backup"}'

# Create with auto-generated name
curl -X POST http://localhost:7171/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{"command": "create"}'
```

The body also accepts a JSON array (`[{"command":"..."}]`) or JSONEachRow format (one JSON object per line) for ClickHouse URL engine compatibility.

Supported commands: `create`, `upload`, `download`, `restore`, `create_remote`, `restore_remote`, `delete`, `clean_broken`.

#### Flag support

Commands support CLI-style flags in the command string (both `--flag=VALUE` and `--flag VALUE` forms):

```bash
# Create with incremental base, RBAC, and configs (CronJob format)
curl -X POST http://localhost:7171/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{"command": "create --diff-from-remote=base-backup --rbac --configs my-backup"}'

# Restore with table filter and table mapping (DAG format)
curl -X POST http://localhost:7171/api/v1/actions \
  -H "Content-Type: application/json" \
  -d '{"command": "restore --table=events.transactions --restore-table-mapping transactions:transactions_DR my-backup"}'
```

| Flag | Commands | Description |
|------|----------|-------------|
| `--table` / `-t` | create, restore, create_remote, restore_remote | Table filter pattern |
| `--diff-from-remote` | create, upload, create_remote | Remote incremental base backup |
| `--restore-table-mapping` | restore, restore_remote | Table rename mapping (`src:dst`). Database prefix auto-inferred from `--table` when mapping lacks dots |
| `--rbac` | create, restore, create_remote, restore_remote | Include RBAC objects |
| `--configs` | create, restore, create_remote, restore_remote | Include config files |
| `--rm` / `--drop` | restore, restore_remote | Drop existing tables before restore |

## Legacy Go compatibility (`/backup/*` routes)

chbackup exposes a second set of API routes under `/backup/*` that match the Go clickhouse-backup API. This allows existing ClickHouse URL engine integration tables (e.g., `system.backup_list`, `system.backup_actions`) and CronJob scripts to work without modification after swapping the Docker image.

### Response format differences

The `/backup/*` routes return **JSONEachRow** (newline-delimited JSON objects) instead of JSON arrays:

```
{"name":"backup1","created":"2024-03-01 12:34:56","size":828848,"location":"remote","required":"","desc":"tar, regular"}
{"name":"backup2","created":"2024-03-02 15:45:30","size":1048576,"location":"local","required":"backup1","desc":"tar, incremental"}
```

Mutation endpoints return Go-style acknowledgment:

```json
{"status": "acknowledged", "operation": "create"}
```

### Status value mapping

| chbackup status | Go `/backup/*` status |
|---|---|
| `running` | `in progress` |
| `completed` | `success` |
| `failed` | `error` |
| `killed` | `cancel` |

### Timestamp format

Go routes use `YYYY-MM-DD HH:MM:SS` (no `T` separator, no timezone) instead of RFC 3339.

### Available routes

| Route | Method | Equivalent chbackup endpoint |
|---|---|---|
| `/backup/list` | GET | `/api/v1/list` (JSONEachRow output) |
| `/backup/list/:where` | GET | `/api/v1/list?location=:where` |
| `/backup/actions` | GET | `/api/v1/actions` (JSONEachRow output) |
| `/backup/actions` | POST | `/api/v1/actions` |
| `/backup/create` | POST | `/api/v1/create` |
| `/backup/create_remote` | POST | `/api/v1/create_remote` |
| `/backup/upload/:name` | POST | `/api/v1/upload/:name` |
| `/backup/download/:name` | POST | `/api/v1/download/:name` |
| `/backup/restore/:name` | POST | `/api/v1/restore/:name` |
| `/backup/restore_remote/:name` | POST | `/api/v1/restore_remote/:name` |
| `/backup/delete/:where/:name` | POST | `DELETE /api/v1/delete/:where/:name` |
| `/backup/clean` | POST | `/api/v1/clean` |
| `/backup/clean/remote_broken` | POST | `/api/v1/clean/remote_broken` |
| `/backup/clean/local_broken` | POST | `/api/v1/clean/local_broken` |
| `/backup/status` | GET | `/api/v1/status` |
| `/backup/kill` | POST | `/api/v1/kill` |
| `/backup/tables` | GET | `/api/v1/tables` |
| `/backup/tables/all` | GET | `/api/v1/tables?all=true` |
| `/backup/version` | GET | `/api/v1/version` |

### Integration table compatibility

chbackup's auto-created integration tables (`system.backup_list`, `system.backup_actions`) now point to the Go-compatible `/backup/list` and `/backup/actions` endpoints. This means:
- `system.backup_list` includes the `desc` column (`"tar, regular"`, `"tar, incremental"`, or `"broken: {reason}"`)
- `system.backup_actions` returns Go status values (`success`, `in progress`, `error`, `cancel`)
- On server startup, existing integration tables are dropped and recreated to pick up any schema/URL changes

Existing tables created by Go clickhouse-backup will also work automatically — the `/backup/*` routes are fully compatible.

## Error responses

All errors return JSON with an `error` field:

| HTTP Code | Meaning |
|-----------|---------|
| 200 | Success |
| 400 | Bad request (missing parameters, invalid name) |
| 404 | Backup not found |
| 423 | Another operation is already running |
| 500 | Internal error (S3 failure, ClickHouse error, etc.) |

Example:

```json
{"error": "backup 'nonexistent' not found"}
```
