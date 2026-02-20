# API Server: Go vs Rust Parity Analysis

**Date**: 2026-02-20
**Go source**: `pkg/server/server.go` (2468 lines), `pkg/server/utils.go`, `pkg/server/callback.go`, `pkg/server/metrics/metrics.go`
**Rust source**: `src/server/` (mod.rs, routes.rs, actions.rs, auth.rs, metrics.rs, state.rs)

---

## 1. Route Paths and HTTP Methods

### Go Route Registration (gorilla/mux)

| Method(s)     | Path                              | Handler                         |
|---------------|-----------------------------------|---------------------------------|
| GET, HEAD     | `/`                               | httpRootHandler                 |
| POST          | `/`                               | httpRestartHandler              |
| POST, GET     | `/restart`                        | httpRestartHandler              |
| GET, HEAD     | `/backup/version`                 | httpVersionHandler              |
| POST, GET     | `/backup/kill`                    | httpKillHandler                 |
| POST, GET     | `/backup/watch`                   | httpWatchHandler                |
| GET           | `/backup/tables`                  | httpTablesHandler               |
| GET           | `/backup/tables/all`              | httpTablesHandler               |
| GET, HEAD     | `/backup/list`                    | httpListHandler                 |
| GET           | `/backup/list/{where}`            | httpListHandler                 |
| POST          | `/backup/create`                  | httpCreateHandler               |
| POST          | `/backup/create_remote`           | httpCreateRemoteHandler         |
| POST          | `/backup/clean`                   | httpCleanHandler                |
| POST          | `/backup/clean/remote_broken`     | httpCleanRemoteBrokenHandler    |
| POST          | `/backup/clean/local_broken`      | httpCleanLocalBrokenHandler     |
| POST          | `/backup/upload/{name}`           | httpUploadHandler               |
| POST          | `/backup/download/{name}`         | httpDownloadHandler             |
| POST          | `/backup/restore/{name}`          | httpRestoreHandler              |
| POST          | `/backup/restore_remote/{name}`   | httpRestoreRemoteHandler        |
| POST          | `/backup/delete/{where}/{name}`   | httpDeleteHandler               |
| GET           | `/backup/status`                  | httpStatusHandler               |
| GET, HEAD     | `/backup/actions`                 | actionsLog                      |
| POST          | `/backup/actions`                 | actions                         |
| GET           | `/health`                         | (inline in registerMetricsHandlers) |
| GET           | `/metrics`                        | promhttp.Handler()              |
| GET           | `/debug/pprof/*`                  | (Go pprof, conditional)         |

### Rust Route Registration (axum)

| Method | Path                              | Handler                |
|--------|-----------------------------------|------------------------|
| GET    | `/health`                         | routes::health         |
| GET    | `/api/v1/version`                 | routes::version        |
| GET    | `/api/v1/status`                  | routes::status         |
| GET    | `/api/v1/actions`                 | routes::get_actions    |
| POST   | `/api/v1/actions`                 | routes::post_actions   |
| GET    | `/api/v1/list`                    | routes::list_backups   |
| POST   | `/api/v1/create`                  | routes::create_backup  |
| POST   | `/api/v1/upload/{name}`           | routes::upload_backup  |
| POST   | `/api/v1/download/{name}`         | routes::download_backup|
| POST   | `/api/v1/restore/{name}`          | routes::restore_backup |
| POST   | `/api/v1/create_remote`           | routes::create_remote  |
| POST   | `/api/v1/restore_remote/{name}`   | routes::restore_remote |
| DELETE | `/api/v1/delete/{location}/{name}`| routes::delete_backup  |
| POST   | `/api/v1/clean/remote_broken`     | routes::clean_remote_broken |
| POST   | `/api/v1/clean/local_broken`      | routes::clean_local_broken  |
| POST   | `/api/v1/kill`                    | routes::kill_op        |
| POST   | `/api/v1/clean`                   | routes::clean          |
| POST   | `/api/v1/reload`                  | routes::reload         |
| POST   | `/api/v1/watch/start`             | routes::watch_start    |
| POST   | `/api/v1/watch/stop`              | routes::watch_stop     |
| GET    | `/api/v1/watch/status`            | routes::watch_status   |
| POST   | `/api/v1/restart`                 | routes::restart        |
| GET    | `/api/v1/tables`                  | routes::tables         |
| GET    | `/metrics`                        | routes::metrics        |

### GAP-01: Route Path Prefix Mismatch (CRITICAL)

**Go uses `/backup/*` prefix; Rust uses `/api/v1/*` prefix.**

The Go project uses paths like `/backup/create`, `/backup/list`, `/backup/actions`, etc. The Rust project uses `/api/v1/create`, `/api/v1/list`, `/api/v1/actions`, etc.

This is a fundamental incompatibility. Any existing tooling, integration tables (ClickHouse URL engine), monitoring scripts, or orchestration tools that target the Go API paths will not work with the Rust API.

**Recommendation**: Add aliases or a compatibility layer that also serves the Go-style `/backup/*` paths. The integration tables in ClickHouse explicitly use `/backup/actions` and `/backup/list` URLs.

### GAP-02: Missing Root Index Endpoint

Go serves `GET /` as an API index showing version + all registered routes, and `POST /` as a restart alias. Rust has no root endpoint.

### GAP-03: Missing `/backup/list/{where}` Path Variant

Go registers `/backup/list/{where}` where `{where}` is `local` or `remote` as a path segment. Rust only has `/api/v1/list` with `?location=local|remote` as a query param. Both approaches work, but the path variant is used by integration tables and existing scripts.

### GAP-04: Missing HEAD Method Support

Go registers HEAD on several endpoints: `/backup/version`, `/backup/list`, `/backup/actions`, `/`. Rust does not explicitly register HEAD methods. Axum may auto-handle HEAD for GET routes, but this should be verified.

### GAP-05: `/backup/tables/all` Separate Path

Go has a dedicated `/backup/tables/all` path that includes system tables (no filtering). Rust handles this via `?all=true` query param on `/api/v1/tables`. Functionally equivalent but path differs.

### GAP-06: Watch Endpoint Structure Mismatch

Go has a single `POST /backup/watch` endpoint that starts a watch loop with parameters passed via query string (watch_interval, full_interval, tables, etc.). Rust has three separate endpoints:
- `POST /api/v1/watch/start`
- `POST /api/v1/watch/stop`
- `GET /api/v1/watch/status`

Go's approach is a single endpoint that accepts `GET` (status) and `POST` (start). Rust's approach is more RESTful but incompatible with Go client tooling.

### GAP-07: Delete Uses Different HTTP Method

Go uses `POST /backup/delete/{where}/{name}`. Rust uses `DELETE /api/v1/delete/{location}/{name}`. The HTTP method differs (POST vs DELETE), which matters for client compatibility.

### GAP-08: Missing `/debug/pprof/*` Endpoints

Go optionally serves Go pprof profiling endpoints when `api.enable_pprof` is true. Rust has no profiling endpoints. This is a minor gap -- pprof is Go-specific, but equivalent Rust profiling could be useful.

---

## 2. Request Parameter Handling

### GAP-09: Query Parameters vs JSON Body (CRITICAL)

**Go uses URL query parameters for ALL operation endpoints. Rust uses JSON request bodies.**

Go example for create:
```
POST /backup/create?table=default.*&diff-from-remote=prev&schema&rbac&name=mybackup
```

Rust expects:
```
POST /api/v1/create
Content-Type: application/json
{"tables": "default.*", "diff_from": "prev", "schema": true, "rbac": true, "backup_name": "mybackup"}
```

This is a fundamental incompatibility. ClickHouse integration tables use URL-encoded query parameters, and existing scripts/tools send query params.

**Recommendation**: Accept BOTH query parameters and JSON body, with query params taking precedence (or merging). This is the standard approach for backward compatibility.

### GAP-10: Flexible Parameter Naming

Go's `getQueryParameter()` function accepts both underscore and hyphen forms of every parameter name (e.g., both `diff-from-remote` and `diff_from_remote`). Rust is strict about parameter naming.

### GAP-11: Missing Parameters on Several Endpoints

**Create endpoint** missing query param support for:
- `diff-from-remote` (Go create supports this; Rust create only has `diff_from`)
- `resume` flag
- `rbac-only`, `configs-only`, `named-collections-only` (Go has both `--rbac` and `--rbac-only`)

**Upload endpoint** missing:
- `table` (table filter during upload)
- `partitions` (partition filter during upload)
- `diff-from` and `diff-from-remote` (Go upload supports both)
- `schema` (schema-only upload)
- `rbac-only`, `configs-only`, `named-collections-only`
- `skip-projections`
- `resume`/`resumable`

**Download endpoint** missing:
- `table` (table filter during download)
- `partitions` (partition filter during download)
- `schema` (schema-only download)
- `rbac-only`, `configs-only`, `named-collections-only`
- `resume`/`resumable`

**Restore endpoint** missing:
- `ignore-dependencies` / `ignore_dependencies`
- `rbac-only`, `configs-only`, `named-collections-only`
- `skip-projections`
- `resume`/`resumable`
- `restore-schema-as-attach` / `restore_schema_as_attach`
- `replicated-copy-to-detached` / `replicated_copy_to_detached`
- `restore-table-mapping` / `restore_table_mapping`

**Restore Remote endpoint** missing same as Restore plus:
- `hardlink-exists-files` / `hardlink_exists_files`

---

## 3. Response Format

### GAP-12: Response Structure Mismatch (CRITICAL)

Go returns JSONEachRow format (newline-delimited JSON, one object per line) for ALL responses, matching ClickHouse's JSONEachRow format requirement. Rust returns standard JSON arrays.

Go async operation response:
```json
{"status":"acknowledged","operation":"create","backup_name":"2024-01-15","operation_id":"uuid-here"}
```

Rust async operation response:
```json
{"id":42,"status":"started"}
```

Key differences:
- Go uses `"status": "acknowledged"`, Rust uses `"status": "started"`
- Go includes `backup_name` and `operation_id` (UUID) in response
- Go does NOT return a numeric `id`; uses UUID `operation_id` instead
- Go's upload response additionally includes `backup_from` and `diff` fields

### GAP-13: Error Response Structure

Go error responses:
```json
{"status":"error","operation":"create","error":"detailed error message"}
```

Rust error responses:
```json
{"error":"detailed error message"}
```

Go includes `status` and `operation` fields in error responses. Rust only includes `error`.

### GAP-14: List Response Field Differences

Go's list response includes:
- `named_collection_size` (Rust omits this entirely)
- `desc` field with DataFormat + Broken + Tags info (Rust's `ListResponse` has `required` but no `desc`)

Rust's `ListResponse` is missing the `desc` field and `named_collection_size` field.

### GAP-15: Actions Log Response Format

Go's `/backup/actions` (GET) returns `ActionRowStatus` objects:
```json
{"command":"create","status":"in progress","start":"2024-01-15T10:00:00","finish":"","error":"","operation_id":"uuid"}
```

Rust returns `ActionResponse`:
```json
{"id":1,"command":"create","start":"2024-01-15T10:00:00+00:00","finish":"","status":"running","error":""}
```

Differences:
- Go has `operation_id` (UUID string); Rust has `id` (numeric)
- Go status values: `"in progress"`, `"success"`, `"cancel"`, `"error"`; Rust: `"running"`, `"completed"`, `"killed"`, `"failed"`
- Go actions GET supports `?last=N` and `?filter=command` query params; Rust does not

### GAP-16: Version Response

Go returns `{"version":"2.x.y"}` (only version field). Rust returns `{"version":"0.1.0","clickhouse_version":"24.3.1.1234"}` (includes CH version). Rust's response has more info, which is fine, but the field names should be verified against integration expectations.

### GAP-17: Status Response

Go's `/backup/status` returns a list of all action statuses (same as actions log). It also supports `?operationid=uuid` to query a specific operation. Rust's `/api/v1/status` returns a single object with `{status, command, start}` for the current operation. These are fundamentally different.

---

## 4. Authentication

### GAP-18: Query Parameter Authentication

Go supports authentication via BOTH:
1. HTTP Basic Auth header (`Authorization: Basic base64(user:pass)`)
2. Query parameters (`?user=username&pass=password`)

Rust only supports HTTP Basic Auth header. The query parameter method is used by ClickHouse integration tables (URL engine tables encode credentials in the URL).

**Recommendation**: Add query parameter authentication support (`?user=` and `?pass=`).

### GAP-19: Auth Bypass for /metrics

Go's `basicAuthMiddleware` logs `/metrics` calls at DEBUG level instead of INFO, but still requires auth. Both Go and Rust apply auth to all endpoints. No gap here, but worth noting.

### GAP-20: CA Certificate (mTLS) Support

Go supports `api.ca_cert_file` for mutual TLS (client certificate verification). The Rust config has `ca_cert_file` field defined but it is not used in the TLS setup (`start_server` only loads `certificate_file` and `private_key_file`).

---

## 5. Concurrent Operation Locking

### GAP-21: HTTP 423 Status Code (Already Fixed)

Both Go and Rust use HTTP 423 (Locked) when `allow_parallel=false` and an operation is already running. This is aligned.

### GAP-22: Parallel Operation Model

Go's concurrency model allows multiple parallel operations when `allow_parallel=true`, tracked individually by command ID. Rust uses a semaphore with effectively unlimited permits when parallel is enabled.

Go tracks each command with a context + cancel function, allowing cancellation of specific commands by name or operation_id. Rust's `kill_current()` only cancels the single `current_op`.

**Gap**: When `allow_parallel=true`, Go can cancel specific operations by name/operation_id. Rust can only cancel whatever is in `current_op`, which means only the most recently started operation can be killed.

### GAP-23: Watch Command Conflict Detection

Go checks `status.Current.CheckCommandInProgress(fullCommand)` to prevent duplicate watch commands even when `allow_parallel=true`. Rust checks `watch_status.active` for the same purpose. Functionally equivalent.

---

## 6. Callback Support

### GAP-24: Callback URLs (MISSING)

Go supports `?callback=URL&callback_retry=N` on all async operation endpoints. When the operation completes, it POSTs a JSON payload (`{status, error, operation_id}`) to each callback URL. Multiple callbacks can be specified.

Rust has no callback support at all.

**Impact**: Medium. Used by some orchestration tools for async notification of backup completion.

---

## 7. Metrics

### GAP-25: Metric Name Prefix

Go uses `clickhouse_backup_*` namespace prefix. Rust uses `chbackup_*` prefix.

**Impact**: Any existing Prometheus alerts, Grafana dashboards, or recording rules referencing `clickhouse_backup_*` metrics will not work.

### GAP-26: Metric Names and Structure

Go metrics (per-command, for create/upload/download/restore/create_remote/restore_remote/delete):
- `clickhouse_backup_successful_{command}s` (Counter)
- `clickhouse_backup_failed_{command}s` (Counter)
- `clickhouse_backup_last_{command}_start` (Gauge, timestamp)
- `clickhouse_backup_last_{command}_finish` (Gauge, timestamp)
- `clickhouse_backup_last_{command}_duration` (Gauge, nanoseconds)
- `clickhouse_backup_last_{command}_status` (Gauge, 0=failed/1=success/2=unknown)

Go general metrics:
- `clickhouse_backup_last_backup_size_local` (Gauge)
- `clickhouse_backup_last_backup_size_remote` (Gauge)
- `clickhouse_backup_number_backups_remote` (Gauge)
- `clickhouse_backup_number_backups_remote_broken` (Gauge)
- `clickhouse_backup_number_backups_local` (Gauge)
- `clickhouse_backup_number_backups_remote_expected` (Gauge)
- `clickhouse_backup_number_backups_local_expected` (Gauge)
- `clickhouse_backup_in_progress_commands` (Gauge)
- `clickhouse_backup_local_data_size` (Gauge)

**Total Go metrics**: 7 commands x 6 metrics + 9 general = 51 metric time series

Rust metrics:
- `chbackup_backup_duration_seconds` (HistogramVec, `operation` label) -- Go uses Gauge per-command, Rust uses Histogram
- `chbackup_backup_size_bytes` (Gauge)
- `chbackup_backup_last_success_timestamp` (Gauge)
- `chbackup_parts_uploaded_total` (IntCounter)
- `chbackup_parts_skipped_incremental_total` (IntCounter)
- `chbackup_errors_total` (IntCounterVec, `operation` label)
- `chbackup_successful_operations_total` (IntCounterVec, `operation` label)
- `chbackup_number_backups_local` (IntGauge)
- `chbackup_number_backups_remote` (IntGauge)
- `chbackup_in_progress` (IntGauge)
- `chbackup_watch_state` (IntGauge)
- `chbackup_watch_last_full_timestamp` (Gauge)
- `chbackup_watch_last_incremental_timestamp` (Gauge)
- `chbackup_watch_consecutive_errors` (IntGauge)

**Missing from Rust vs Go**:
- Per-command `last_start` timestamp
- Per-command `last_finish` timestamp
- Per-command `last_duration` (as Gauge in nanoseconds; Rust uses Histogram in seconds instead -- different but arguably better)
- Per-command `last_status` (0/1/2 gauge)
- `last_backup_size_local` (Rust only has a single `backup_size_bytes`, not split local/remote)
- `last_backup_size_remote`
- `number_backups_remote_broken`
- `number_backups_remote_expected`
- `number_backups_local_expected`
- `local_data_size`

**Extra in Rust** (not in Go):
- `chbackup_parts_uploaded_total`
- `chbackup_parts_skipped_incremental_total`
- Watch-specific gauges (Go tracks watch through the standard command status)

### GAP-27: Metrics Endpoint Conditionality

Go: `/metrics` endpoint only registered when `enable_metrics=true`. When disabled, the path returns 404.
Rust: `/metrics` endpoint always registered, returns 501 when disabled.

Minor behavioral difference.

---

## 8. Health Endpoint

### GAP-28: Health Response Case

Go returns `{"status":"OK"}` (uppercase OK). Rust returns `{"status":"ok"}` (lowercase ok).

The CLAUDE.md says Go parity was achieved with `{"status":"ok"}` -- but the actual Go source uses uppercase `"OK"`.

---

## 9. Integration Tables

### GAP-29: Third Integration Table (backup_version)

Go creates three integration tables: `system.backup_actions`, `system.backup_list`, and `system.backup_version`. Rust only creates two (actions and list), missing `backup_version`.

### GAP-30: Integration Table Column Mismatch

Go's `system.backup_actions` includes `operation_id String` column. Rust's integration tables likely don't include this since the Rust ActionResponse uses numeric `id` instead of UUID `operation_id`.

Go's `system.backup_list` includes `named_collection_size UInt64` and `desc String` columns that Rust's ListResponse lacks.

---

## 10. Config Reload Behavior

### GAP-31: Config Reload Per-Request

Go calls `api.ReloadConfig()` at the START of every handler (create, upload, download, restore, list, tables, clean, etc.). This means each request always uses the latest config from disk.

Rust only reloads config explicitly via `POST /api/v1/restart` or `POST /api/v1/reload` (watch loop only). Normal request handlers use the config from `ArcSwap` which was loaded at startup or last restart.

**Impact**: In Go, you can edit the config file and the next API call picks it up automatically. In Rust, you must explicitly call restart or reload.

---

## 11. POST /backup/actions Dispatch

### GAP-32: Actions Dispatch Completeness (CRITICAL)

Go's `POST /backup/actions` is a full command dispatcher. It parses the command string using `shlex.Split()` and dispatches to the actual implementation (not just logging). It supports:
- `watch` command with full parameter parsing
- `clean`, `clean_local_broken`, `clean_remote_broken` (synchronous)
- `kill` (synchronous)
- `create`, `restore`, `upload`, `download`, `create_remote`, `restore_remote`, `list` (async via goroutine)
- `delete` (synchronous)

Each dispatched command runs with full parameter support and proper metrics.

Rust's `POST /api/v1/actions` accepts the command string but **does not actually execute it**. The spawned task just calls `state_clone.finish_op(id)` immediately:
```rust
tokio::spawn(async move {
    tracing::info!(command = %command, "Action dispatched from POST /api/v1/actions");
    // For now, we mark the operation as completed immediately.
    state_clone.finish_op(id).await;
});
```

This is documented as a stub: "Full dispatch to actual command functions is wired via the dedicated POST endpoints."

**Impact**: HIGH. The ClickHouse integration table `system.backup_actions` relies on INSERT (POST) to dispatch commands. Without proper dispatch, `INSERT INTO system.backup_actions (command) VALUES ('create_remote mybackup')` does nothing useful.

---

## 12. Status Endpoint

### GAP-33: Status Endpoint Semantics

Go's `GET /backup/status` returns all action statuses (like the actions log) and supports `?operationid=uuid` filtering.

Rust's `GET /api/v1/status` returns only the currently running operation.

---

## 13. Kill Endpoint

### GAP-34: Kill Command Filtering

Go's `/backup/kill` supports `?command=commandname` to kill a specific command by name. It also supports killing all commands when no filter is given.

Rust's `/api/v1/kill` only kills whatever is currently running (no filtering).

### GAP-35: Kill HTTP Methods

Go accepts both POST and GET on `/backup/kill`. Rust only accepts POST.

---

## 14. CORS Handling

### GAP-36: No CORS Headers

Neither Go nor Rust implement CORS headers explicitly. Go sets `Cache-Control` and `Pragma` headers on responses. Rust relies on axum defaults. Not a gap between the two implementations.

---

## 15. Restart Behavior

### GAP-37: Restart Semantics

Go's restart actually stops and restarts the entire HTTP server (closes listener, re-registers routes, starts new listener). It also cancels all running operations and re-creates integration tables.

Rust's restart reloads config, creates new clients, pings CH, and atomically swaps via ArcSwap. It does NOT restart the HTTP listener or cancel running operations.

### GAP-38: Restart HTTP Method

Go accepts both POST and GET on `/restart`. Rust only accepts POST.

### GAP-39: Restart Response Status Code

Go returns HTTP 201 (Created) for restart. Rust returns HTTP 200 (OK).

---

## 16. Backup Name Sanitization

### GAP-40: Backup Name Cleaning

Go uses `utils.CleanBackupNameRE` regex to sanitize backup names from user input, stripping dangerous characters. Rust does not appear to sanitize backup names received via API.

---

## 17. Operation ID

### GAP-41: UUID Operation ID vs Numeric ID

Go uses UUID v1 (`google/uuid`) as operation IDs. These are returned in responses and used for status queries.

Rust uses monotonically increasing u64 IDs from the ActionLog.

This affects:
- Response format (operation_id vs id)
- Status query semantics (operationid= query param)
- Integration table schema (operation_id column)

---

## Summary of Gaps by Priority

### CRITICAL (breaking for integration table / existing tooling compatibility)

| # | Gap | Description |
|---|-----|-------------|
| 01 | Route paths | `/backup/*` vs `/api/v1/*` prefix mismatch |
| 09 | Query params vs JSON body | Go uses query params; Rust uses JSON body |
| 12 | Response format | JSONEachRow vs JSON array; field differences |
| 32 | Actions dispatch stub | POST /actions does not execute commands |

### HIGH (significant behavioral differences)

| # | Gap | Description |
|---|-----|-------------|
| 18 | Query param auth | Missing `?user=&pass=` for integration tables |
| 25 | Metric prefix | `clickhouse_backup_*` vs `chbackup_*` |
| 26 | Metric structure | Many Go metrics missing in Rust |
| 31 | Config reload per-request | Go auto-reloads; Rust requires explicit restart |
| 14 | List `desc` field | Missing `desc` and `named_collection_size` in list response |

### MEDIUM (functionality differences)

| # | Gap | Description |
|---|-----|-------------|
| 02 | Root index | No `GET /` with version and routes |
| 03 | List path variant | No `/backup/list/{where}` path |
| 06 | Watch endpoint structure | Different watch lifecycle model |
| 07 | Delete HTTP method | POST vs DELETE |
| 11 | Missing endpoint parameters | Many operation query params missing |
| 24 | Callback URLs | No async completion callbacks |
| 29 | backup_version table | Third integration table missing |
| 33 | Status semantics | Different data returned |
| 34 | Kill filtering | No command-name filter on kill |
| 37 | Restart behavior | Different restart semantics |
| 40 | Name sanitization | No backup name cleaning |
| 41 | UUID vs numeric ID | Different operation ID schemes |

### LOW (minor differences)

| # | Gap | Description |
|---|-----|-------------|
| 04 | HEAD method | May need explicit HEAD handlers |
| 05 | Tables /all path | Different approach (path vs query param) |
| 08 | pprof endpoints | Go-specific profiling, not applicable |
| 10 | Flexible param naming | Underscore/hyphen interchangeability |
| 13 | Error response fields | Missing `status` and `operation` in errors |
| 15 | Actions log params | Missing `?last=N` and `?filter=` |
| 16 | Version response | Extra CH version field (additive, OK) |
| 17 | Status response | Simplified vs full status |
| 20 | mTLS ca_cert_file | Config field defined but unused |
| 22 | Parallel kill | Can't kill specific ops when parallel |
| 27 | Metrics disabled behavior | 404 vs 501 when disabled |
| 28 | Health status case | "OK" vs "ok" |
| 30 | Integration table columns | Column mismatch |
| 35 | Kill methods | POST-only vs POST+GET |
| 36 | CORS | Neither implements CORS |
| 38 | Restart methods | POST-only vs POST+GET |
| 39 | Restart status code | 200 vs 201 |
