# CLAUDE.md -- src/server

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the HTTP API server for chbackup using axum (design doc section 9). It enables Kubernetes sidecar operation with endpoints for creating, uploading, downloading, and restoring backups, plus health checks, action logging, ClickHouse integration tables, and watch mode lifecycle management.

## Directory Structure

```
src/server/
  mod.rs          -- build_router(), start_server(), parse_integration_host_port(), re-exports
  routes.rs       -- All endpoint handler functions (~20 endpoints), request/response types, metrics instrumentation
  actions.rs      -- ActionLog ring buffer + ActionEntry/ActionStatus types
  auth.rs         -- Basic auth middleware (HTTP Basic, optional based on config)
  metrics.rs      -- Metrics struct (custom prometheus::Registry), 14 metric families, encode()
  state.rs        -- AppState, RunningOp, operation management, auto-resume on restart
```

## Key Patterns

### AppState Sharing (state.rs)
`AppState` is the central shared state for all axum handlers, extracted via `State<AppState>`. It must be `Clone` for axum. All inner fields are `Arc`-wrapped or natively `Clone`:
- `config: Arc<Config>` -- immutable configuration
- `ch: ChClient` -- ClickHouse client (Clone)
- `s3: S3Client` -- S3 client (Clone)
- `action_log: Arc<Mutex<ActionLog>>` -- operation history ring buffer
- `current_op: Arc<Mutex<Option<RunningOp>>>` -- currently running operation for kill support
- `op_semaphore: Arc<Semaphore>` -- concurrency control (1 permit when `allow_parallel=false`)
- `metrics: Option<Arc<Metrics>>` -- Prometheus metrics registry (`None` when `config.api.enable_metrics` is false)
- `watch_shutdown_tx: Option<watch::Sender<bool>>` -- watch loop shutdown signal (`None` when watch inactive)
- `watch_reload_tx: Option<watch::Sender<bool>>` -- watch loop config reload signal (`None` when watch inactive)
- `watch_status: Arc<Mutex<WatchStatus>>` -- shared watch loop status for API queries
- `config_path: PathBuf` -- path to config file, used for config reload

Uses `tokio::sync::Mutex` (not `std::sync::Mutex`) since locks are held across `.await` points.

### Operation Lifecycle (state.rs)
Every mutating operation follows a three-phase lifecycle:
1. **Start**: `try_start_op(command)` acquires a semaphore permit (non-blocking via `try_acquire_owned`), creates a `CancellationToken`, logs the start in `ActionLog`, stores `RunningOp`
2. **Execute**: Background `tokio::spawn` task runs the actual command function
3. **Complete**: One of three exit paths:
   - `finish_op(id)` -- success: marks `ActionStatus::Completed`, clears `RunningOp`
   - `fail_op(id, error)` -- failure: marks `ActionStatus::Failed`, clears `RunningOp`
   - `kill_current()` -- cancellation: cancels token, marks `ActionStatus::Killed`, clears `RunningOp`

The `OwnedSemaphorePermit` is stored inside `RunningOp` and dropped when the operation completes, releasing the permit for the next operation.

### Route Handler Delegation Pattern (routes.rs)
All operation endpoints follow the same pattern:
```rust
async fn handler(State(state): State<AppState>, ...) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let (id, _token) = state.try_start_op("command")
        .map_err(|e| (StatusCode::CONFLICT, Json(ErrorResponse { error: e.to_string() })))?;
    let state_clone = state.clone();
    tokio::spawn(async move {
        let result = do_operation(&state_clone).await;
        match result {
            Ok(_) => state_clone.finish_op(id).await,
            Err(e) => state_clone.fail_op(id, e.to_string()).await,
        }
    });
    Ok(Json(OperationStarted { id, status: "started".into() }))
}
```
- Returns 200 immediately with action ID (async operation)
- Returns 409 Conflict if another operation is running (when `allow_parallel=false`)
- Request bodies use `Option<Json<T>>` to handle empty bodies gracefully

### ActionLog Ring Buffer (actions.rs)
Bounded `VecDeque<ActionEntry>` with configurable capacity (default 100). Tracks all server operations with monotonic IDs. When at capacity, oldest entry is popped on new start. Status lifecycle: `Running` -> `Completed` | `Failed(String)` | `Killed`.

`ActionEntry` and `ActionStatus` derive `Serialize` for JSON API responses. `ActionStatus` uses `#[serde(rename_all = "snake_case")]` for JSON output.

### Basic Auth Middleware (auth.rs)
Conditionally applied to the entire router when `config.api.username` AND `config.api.password` are both non-empty. Uses axum's `middleware::from_fn_with_state`. Decodes `Authorization: Basic <base64>` header via `base64::engine::general_purpose::STANDARD`. Returns 401 with `WWW-Authenticate: Basic` header on failure.

### Compound Operations
- `create_remote`: chains `backup::create()` then `upload::upload()` in a single spawned task; passes `rbac`, `configs`, `named_collections` flags to `create()` (Phase 4e)
- `restore_remote`: chains `download::download()` then `restore::restore()` in a single spawned task; passes `rename_as`, `database_mapping` remap parameters and `rbac`, `configs`, `named_collections` flags to the restore step (Phase 4a, 4e)
- If the first step fails, the operation is marked as failed and the second step is skipped

### Restore Remap Parameters (routes.rs, Phase 4a)
Both restore endpoints accept remap parameters for table/database renaming:

- **`RestoreRequest`** (for `POST /api/v1/restore/{name}`): Fields: `tables`, `schema`, `data_only`, `rename_as` (optional, `--as` flag value), `database_mapping` (optional, `-m` flag value as `"src:dst,..."` string), `rm`, `rbac` (optional), `configs` (optional), `named_collections` (optional). The `database_mapping` string is parsed via `remap::parse_database_mapping()` inside the spawned task; parse errors cause immediate `fail_op`.
- **`RestoreRemoteRequest`** (for `POST /api/v1/restore_remote/{name}`): Fields: `tables`, `schema`, `data_only`, `rename_as` (optional), `database_mapping` (optional), `rm` (optional, Phase 4d -- Mode A destructive restore), `rbac` (optional), `configs` (optional), `named_collections` (optional). All optional fields use `#[serde(default)]` for backward compatibility.

Auto-resume (`auto_resume()` in state.rs) passes `None` for both `rename_as` and `database_mapping`, `false` for `rm`, and `false` for `rbac`, `configs`, `named_collections` since resume restores to original names and should never drop tables or re-apply RBAC/configs.

### Auto-Resume on Restart (state.rs)
When `config.api.complete_resumable_after_restart` is true, `auto_resume()` scans `{data_path}/backup/` for state files (`upload.state.json`, `download.state.json`, `restore.state.json`) and spawns corresponding operations with `resume=true`. Operations go through `try_start_op()` respecting concurrency limits. Small delay (100ms) between spawned operations.

### Integration Tables (clickhouse/client.rs, wired in mod.rs)
On startup (if `config.api.create_integration_tables` is true), creates two ClickHouse URL engine tables:
- `system.backup_list` -- SELECT queries proxied to `GET /api/v1/list` (JSONEachRow)
- `system.backup_actions` -- SELECT/INSERT queries proxied to `GET/POST /api/v1/actions` (JSONEachRow)

On graceful shutdown, both tables are dropped. Creation failures are logged as warnings but do not prevent server startup.

### TLS Support (mod.rs)
When `config.api.secure` is true, uses `axum_server::bind_rustls` with certificates from `config.api.certificate_file` and `config.api.private_key_file`. Otherwise uses plain `axum::serve` with `tokio::net::TcpListener`.

### Graceful Shutdown (mod.rs)
Listens for Ctrl+C (SIGINT). On shutdown:
1. Sends shutdown signal to watch loop (if active)
2. Drops integration tables (if they were created)
3. Stops accepting new connections
4. Logs "Server stopped"

### Prometheus Metrics (metrics.rs)
A `Metrics` struct holds a custom (non-global) `prometheus::Registry` and 14 metric families, all prefixed with `chbackup_`. Created conditionally in `AppState::new()` based on `config.api.enable_metrics`.

**Metric families:**
- `chbackup_backup_duration_seconds` -- HistogramVec with `operation` label (create, upload, download, restore, create_remote, restore_remote, delete, clean_broken_remote, clean_broken_local, clean)
- `chbackup_backup_size_bytes` -- Gauge (last backup compressed size)
- `chbackup_backup_last_success_timestamp` -- Gauge (Unix timestamp)
- `chbackup_parts_uploaded_total` -- IntCounter
- `chbackup_parts_skipped_incremental_total` -- IntCounter
- `chbackup_errors_total` -- IntCounterVec with `operation` label
- `chbackup_successful_operations_total` -- IntCounterVec with `operation` label
- `chbackup_number_backups_local` -- IntGauge (refreshed on scrape)
- `chbackup_number_backups_remote` -- IntGauge (refreshed on scrape)
- `chbackup_in_progress` -- IntGauge (1 if running, 0 otherwise)
- `chbackup_watch_state` -- IntGauge (Phase 3d, defaults to 0)
- `chbackup_watch_last_full_timestamp` -- Gauge (Phase 3d, defaults to 0)
- `chbackup_watch_last_incremental_timestamp` -- Gauge (Phase 3d, defaults to 0)
- `chbackup_watch_consecutive_errors` -- IntGauge (Phase 3d, defaults to 0)

**Scrape-time refresh:** The `/metrics` handler calls `refresh_backup_counts()` which uses `spawn_blocking` for `list_local()` and async for `list_remote()` to update backup count gauges. The `in_progress` gauge is computed from `current_op` state.

**Operation instrumentation:** Each spawned task in `routes.rs` records:
- Duration via `backup_duration_seconds.with_label_values(&[op]).observe(elapsed)`
- Success via `successful_operations_total.with_label_values(&[op]).inc()`
- Failure via `errors_total.with_label_values(&[op]).inc()`
- For create: `backup_size_bytes.set(manifest.compressed_size)` and `backup_last_success_timestamp.set(now)`

### Clean Endpoint (routes.rs)
`POST /api/v1/clean` -- Shadow directory cleanup. Follows the standard operation lifecycle pattern (try_start_op -> spawn -> finish_op/fail_op). Calls `list::clean_shadow(&ch, &data_path, None)` to remove `chbackup_*` directories from all disk shadow paths. Records `clean` operation label for duration, success, and error metrics.

### Watch Mode Integration (Phase 3d)

**WatchStatus struct** (state.rs): Shared between the watch loop and API handlers via `Arc<Mutex<WatchStatus>>`. Fields: `active`, `state` (string), `last_full`, `last_incr`, `consecutive_errors`, `next_backup_in`.

**Watch loop spawn** (mod.rs): When `--watch` flag or `config.watch.enabled` is set, `start_server()` creates shutdown/reload channels, builds a `WatchContext`, and spawns `watch::run_watch_loop()` as a tokio background task. On server shutdown (Ctrl+C), the shutdown signal is sent to the watch loop.

**SIGHUP handler** (mod.rs): Unix-only (`#[cfg(unix)]`). Spawns a task that listens for `SIGHUP` signals and sends `true` on the `reload_tx` channel, triggering config hot-reload in the watch loop. On non-Unix platforms, use the `/api/v1/reload` API endpoint instead.

**spawn_watch_from_state()** (mod.rs): Creates new channels and a `WatchContext` from the current `AppState`, spawns the watch loop. Used by the `watch_start` API endpoint to start watch dynamically.

**Watch API endpoints** (routes.rs -- replaced stubs):
- `POST /api/v1/watch/start` -- Start watch loop; returns 409 if already active
- `POST /api/v1/watch/stop` -- Stop watch loop via shutdown signal; returns 404 if not active
- `GET /api/v1/watch/status` -- Returns JSON with state, last_full, last_incr, consecutive_errors, next_in
- `POST /api/v1/reload` -- Sends reload signal to watch loop (or re-reads config if watch inactive)

### Stub Endpoints
Endpoints for future phases return 501 Not Implemented:
- `/api/v1/restart` (server restart logic)
- `/api/v1/tables` (Phase 4f)

### Sync Function Handling
Sync functions from `list` module (`delete_local`, `clean_broken_local`) are called via `tokio::task::spawn_blocking` to avoid blocking the async runtime.

### RBAC/Config/Named Collections Request Fields (Phase 4e)
All four operation request types (`CreateRequest`, `RestoreRequest`, `CreateRemoteRequest`, `RestoreRemoteRequest`) include three optional boolean fields added in Phase 4e:
- `rbac: Option<bool>` -- Enable RBAC backup/restore (default: false via `unwrap_or(false)`)
- `configs: Option<bool>` -- Enable config file backup/restore (default: false)
- `named_collections: Option<bool>` -- Enable named collections backup/restore (default: false)

These fields are `Option<bool>` for backward compatibility -- existing API clients that omit these fields get `None`, which defaults to `false`. The `*_backup_always` config overrides still apply on the implementation side.

### Response Types for Integration Tables
- `ListResponse` matches ALL columns of `system.backup_list` (name, created, location, size, data_size, object_disk_size, metadata_size, rbac_size, config_size, compressed_size, required)
- `ActionResponse` matches ALL columns of `system.backup_actions` (command, start, finish, status, error)
- Both derive `Serialize` + `Deserialize` for bidirectional JSON compatibility with ClickHouse URL engine

### Public API
- `build_router(state: AppState) -> Router` -- Assembles all routes with optional auth middleware
- `start_server(config: Arc<Config>, ch: ChClient, s3: S3Client, watch: bool, config_path: PathBuf) -> Result<()>` -- Full server lifecycle: state creation, router build, optional watch loop spawn, integration tables, auto-resume, listen, graceful shutdown
- `spawn_watch_from_state(state: &mut AppState, config_path: PathBuf, macros: HashMap<String, String>)` -- Spawn watch loop from API context (used by watch_start endpoint)
- `AppState::new(config, ch, s3, config_path) -> Self` -- Create shared state with semaphore, optional metrics, and watch fields initialized to None/default
- `AppState::try_start_op(command) -> Result<(u64, CancellationToken), &str>` -- Start tracked operation
- `AppState::finish_op(id)` / `fail_op(id, error)` / `kill_current() -> bool` -- Operation exit paths
- `scan_resumable_state_files(data_path) -> Vec<ResumableOp>` -- Find interrupted operations
- `auto_resume(state)` -- Spawn resume tasks for found state files
- `Metrics::new() -> Result<Self, prometheus::Error>` -- Create metrics registry with all 14 families registered
- `Metrics::encode() -> Result<String, prometheus::Error>` -- Encode all metrics to Prometheus text exposition format

### Error Handling
- Handler errors return `(StatusCode, Json<ErrorResponse>)` tuples
- 409 Conflict for concurrent operation rejection
- 400 Bad Request for invalid inputs (unknown location, empty command)
- 401 Unauthorized for auth failures
- 404 Not Found for kill with no running operation
- 501 Not Implemented for stub endpoints
- Integration table DDL failures are warnings, not fatal errors

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
