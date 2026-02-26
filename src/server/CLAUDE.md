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
  state.rs        -- AppState, RunningOp, run_operation helper, validate_backup_name, operation management, auto-resume on restart
```

## Key Patterns

### AppState Sharing with ArcSwap (state.rs)
`AppState` is the central shared state for all axum handlers, extracted via `State<AppState>`. It must be `Clone` for axum. All inner fields are `Arc`-wrapped or natively `Clone`:
- `config: Arc<ArcSwap<Config>>` -- hot-swappable configuration (Phase 5)
- `ch: Arc<ArcSwap<ChClient>>` -- hot-swappable ClickHouse client (Phase 5)
- `s3: Arc<ArcSwap<S3Client>>` -- hot-swappable S3 client (Phase 5)
- `action_log: Arc<Mutex<ActionLog>>` -- operation history ring buffer
- `running_ops: Arc<Mutex<HashMap<u64, RunningOp>>>` -- all currently running operations, keyed by action ID, for kill support and parallel tracking
- `op_semaphore: Arc<Semaphore>` -- concurrency control (1 permit when `allow_parallel=false`)
- `metrics: Option<Arc<Metrics>>` -- Prometheus metrics registry (`None` when `config.api.enable_metrics` is false)
- `watch_shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>` -- watch loop shutdown signal (`None` when watch inactive); mutex ensures `spawn_watch_from_state` updates are visible to all axum handler clones
- `watch_reload_tx: Arc<Mutex<Option<watch::Sender<bool>>>>` -- watch loop config reload signal (`None` when watch inactive)
- `watch_status: Arc<Mutex<WatchStatus>>` -- shared watch loop status for API queries
- `config_path: PathBuf` -- path to config file, used for config reload
- `manifest_cache: Arc<Mutex<ManifestCache>>` -- in-memory TTL-based cache for remote backup summaries

Uses `tokio::sync::Mutex` (not `std::sync::Mutex`) since locks are held across `.await` points.

**ArcSwap access pattern** (Phase 5): Handlers read config/clients via `.load()` which returns a `Guard<Arc<T>>` that derefs to `T`. The `/api/v1/restart` and `/api/v1/reload` endpoints atomically swap new values via `.store(Arc::new(new_value))`. All handler call sites use `state.config.load()`, `state.ch.load()`, `state.s3.load()` instead of direct field access. `load_full()` is used in `run_operation()` and `spawn_watch_from_state()` where an owned `Arc<T>` is needed (for cloning into background tasks).

### Operation Lifecycle (state.rs)
Every mutating operation follows a three-phase lifecycle:
1. **Start**: `try_start_op(command, backup_name)` acquires a semaphore permit (non-blocking via `try_acquire_owned`), checks for a same-name conflict in `running_ops` when `backup_name` is `Some` (rejects with 423 when `allow_parallel=true` and same backup is already running), creates a `CancellationToken`, logs the start in `ActionLog`, inserts `RunningOp` into `running_ops` HashMap keyed by action ID
2. **Execute**: Background `tokio::spawn` task runs the actual command function, wrapped in `tokio::select!` with a cancellation branch
3. **Complete**: One of three exit paths:
   - `finish_op(id)` -- success: marks `ActionStatus::Completed`, removes `RunningOp` from HashMap
   - `fail_op(id, error)` -- failure: marks `ActionStatus::Failed`, removes `RunningOp` from HashMap
   - `kill_op(id: Option<u64>)` -- cancellation: cancels token(s), marks `ActionStatus::Killed`, removes `RunningOp`(s) from HashMap. If `id` is `Some(N)`, cancels only that operation; if `None`, cancels ALL running operations (backward-compatible kill-all)

The `OwnedSemaphorePermit` is stored inside `RunningOp` and dropped when the operation completes, releasing the permit for the next operation. Multiple operations can be tracked simultaneously when `allow_parallel=true`.

### Route Handler Delegation Pattern (routes.rs)
All 10 standalone operation endpoints use the `run_operation()` DRY helper (defined in `state.rs`):
```rust
async fn handler(State(state): State<AppState>, ...) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    validate_backup_name(&name).map_err(|e| (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e.to_string() })))?;
    run_operation(&state, "command", "op_label", invalidate_cache, |config, ch, s3| async move {
        // operation-specific logic
        Ok(())
    }).await
}
```

The `run_operation()` helper encapsulates:
- `try_start_op()` with 423 error mapping
- `tokio::spawn` with `tokio::select!` (cancellation branch via `CancellationToken`)
- Duration, success, and error metrics recording
- `finish_op()`/`fail_op()` lifecycle calls
- Optional `ManifestCache` invalidation (when `invalidate_cache=true`)
- Returns 200 immediately with action ID

**Exclusion**: `post_actions` is intentionally excluded from the `run_operation()` helper because it returns `(StatusCode, Json<OperationStarted>)` (200 OK with action ID), which is incompatible with the helper's return type. `post_actions` retains its inline `try_start_op` + `tokio::spawn` + `tokio::select!` pattern.

### Kill Endpoint (routes.rs)
`POST /api/v1/kill` -- Cancel running operation(s). Accepts optional `?id=N` query parameter via `KillParams { id: Option<u64> }`.

- If `?id=N` is provided: cancels only the operation with that action ID
- If no `id` is provided: cancels ALL running operations (backward-compatible kill-all)
- Returns 200 `"killed"` on success, 404 if no matching operation found

**CancellationToken wiring**: All 11 route handlers (10 via `run_operation()` + `post_actions`) wire `CancellationToken` into spawned tasks via `tokio::select!`. When the token is cancelled, the operation's future is DROPPED -- this aborts the task but does not run cleanup inside the function. Known limitation: in-progress FREEZE operations will leave shadow directories that can be cleaned via `chbackup clean`.

**Auto-resume cancellation**: Auto-resume operations (upload/download/restore) use the real `CancellationToken` from `try_start_op()` and wrap the operation in `tokio::select!`, so `/api/v1/kill` stops them like any other operation. They also acquire PID locks before running (design §2 — all mutating commands hold a PID lock).

### Backup Name Validation (state.rs)
`validate_backup_name(name: &str) -> Result<(), &'static str>` prevents path traversal attacks via malicious backup names. Rejects names that are:
- Empty
- Contain `..` (parent directory traversal)
- Contain `/` or `\` (path separators)
- Contain NUL bytes

NOTE: `validate_backup_name` does NOT reject `"latest"` or `"previous"`. Those reserved shortcut names are allowed through validation so CLI commands like `upload latest` can be resolved after validation. The reserved-name check (which prevents creating a backup *named* "latest") lives only in `resolve_backup_name()` in `main.rs`, which is used exclusively for create/create_remote commands.

Wired into all API entry points that accept backup names (called BEFORE `try_start_op` to avoid consuming a semaphore permit for invalid requests) and CLI entry points (`resolve_backup_name()` and `backup_name_required()` in `main.rs`). Returns 400 Bad Request with descriptive error message on validation failure.

### POST /api/v1/actions Command Dispatch (routes.rs)
`post_actions()` accepts a `Vec<ActionRequest>` body, extracts the first element's `command` field, splits on whitespace, and dispatches based on the first word. Supported commands: `create`, `upload`, `download`, `restore`, `create_remote`, `restore_remote`, `delete`, `clean_broken`. Each branch validates backup names, loads config/ch/s3 via `state.config.load()` etc., calls the corresponding command function with default parameters, records duration, and calls `finish_op`/`fail_op`. The backup name defaults to the second word (or UTC timestamp if missing). Returns 400 for unknown commands or empty body, 423 if another operation is running. Uses inline `tokio::select!` for cancellation (not `run_operation()` helper due to incompatible return type).

### ActionLog Ring Buffer (actions.rs)
Bounded `VecDeque<ActionEntry>` with configurable capacity (default 100). Tracks all server operations with monotonic IDs. When at capacity, oldest entry is popped on new start. Status lifecycle: `Running` -> `Completed` | `Failed(String)` | `Killed`.

`ActionEntry` and `ActionStatus` derive `Serialize` for JSON API responses. `ActionStatus` uses `#[serde(rename_all = "snake_case")]` for JSON output.

### Basic Auth Middleware (auth.rs)
Always applied unconditionally to the entire router via `middleware::from_fn_with_state`. The middleware reads live config on every request: when both `config.api.username` AND `config.api.password` are empty, requests pass through without authentication. This allows auth to be enabled at runtime via `/api/v1/restart` without rebuilding the router. Decodes `Authorization: Basic <base64>` header via `base64::engine::general_purpose::STANDARD`. Credential comparison uses constant-time `constant_time_eq()` to prevent timing attacks. Returns 401 with `WWW-Authenticate: Basic` header on failure.

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
A `shutdown_signal()` async helper uses `tokio::select!` to wait for either SIGINT (Ctrl+C) or SIGTERM (Unix only, via `SignalKind::terminate()`). On non-Unix platforms, only SIGINT is handled. This enables Kubernetes `kubectl delete pod` (which sends SIGTERM) to trigger the same graceful shutdown as Ctrl+C. On shutdown:
1. Sends shutdown signal to watch loop (if active)
2. Drops integration tables (if they were created)
3. Stops accepting new connections
4. Logs "Server stopped"

The `shutdown_signal()` helper is used in both the TLS (`axum_server::bind_rustls`) and plain (`axum::serve`) server paths.

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

**Scrape-time refresh:** The `/metrics` handler calls `refresh_backup_counts()` which uses `spawn_blocking` for `list_local()` and async for `list_remote()` to update backup count gauges. The `in_progress` gauge is computed from `running_ops` HashMap state (`!running_ops.is_empty()`).

**Operation instrumentation:** The `run_operation()` helper records generic metrics (duration, success, error) for all operations. Operation-specific metrics (e.g. `backup_size_bytes`, `backup_last_success_timestamp` for create) are recorded inside the closure.

### Clean Endpoint (routes.rs)
`POST /api/v1/clean` -- Shadow directory cleanup. Uses `run_operation()` helper. Calls `list::clean_shadow(&ch, &data_path, None)` to remove `chbackup_*` directories from all disk shadow paths. Records `clean` operation label for duration, success, and error metrics.

### Watch Mode Integration (Phase 3d)

**WatchStatus struct** (state.rs): Shared between the watch loop and API handlers via `Arc<Mutex<WatchStatus>>`. Fields: `active`, `state` (string), `last_full`, `last_incr`, `consecutive_errors`, `next_backup_in`.

**Watch loop spawn** (mod.rs): When `--watch` flag or `config.watch.enabled` is set, `start_server()` creates shutdown/reload channels, builds a `WatchContext`, and spawns `watch::run_watch_loop()` as a tokio background task. On server shutdown (Ctrl+C), the shutdown signal is sent to the watch loop. CLI flags `--watch-interval` and `--full-interval` on the `server` command override `config.watch.watch_interval` and `config.watch.full_interval` before server startup (wired in `main.rs`).

**SIGHUP handler** (mod.rs): Unix-only (`#[cfg(unix)]`). Spawns a task that listens for `SIGHUP` signals and sends `true` on the `reload_tx` channel, triggering config hot-reload in the watch loop. On non-Unix platforms, use the `/api/v1/reload` API endpoint instead.

**spawn_watch_from_state()** (mod.rs): Creates new channels and a `WatchContext` from the current `AppState`, spawns the watch loop. Used by the `watch_start` API endpoint to start watch dynamically.

**Watch API endpoints** (routes.rs -- replaced stubs):
- `POST /api/v1/watch/start` -- Start watch loop; accepts optional `WatchStartRequest` JSON body with `watch_interval` and `full_interval` overrides; validates merged config before spawning; returns 423 if already active, 400 if interval validation fails
- `POST /api/v1/watch/stop` -- Stop watch loop via shutdown signal; returns 404 if not active
- `GET /api/v1/watch/status` -- Returns JSON with state, last_full, last_incr, consecutive_errors, next_in
- `POST /api/v1/reload` -- Reloads config and creates new clients via `reload_config_and_clients()` helper; also sends reload signal to watch loop if active

### Reload Endpoint (routes.rs)
`POST /api/v1/reload` -- Reloads config from disk and creates new `ChClient` and `S3Client` via the shared `reload_config_and_clients()` helper, then atomically swaps all three in `AppState` via `ArcSwap::store()`. If the watch loop is active, also sends the reload signal. Returns `ReloadResponse { status: "reloaded" }`. Unlike restart, reload does NOT ping ClickHouse before swapping.

**`reload_config_and_clients()` helper** (routes.rs): Shared between `reload()` and `restart()`. Loads `Config::load(&state.config_path, &[])`, calls `validate()`, creates new `ChClient::new(&config.clickhouse)` and `S3Client::new(&config.s3).await`. Returns `(Config, ChClient, S3Client)` tuple without performing the swap -- callers decide when/whether to swap (restart adds a ping gate).

### Restart Endpoint (routes.rs)
`POST /api/v1/restart` -- Calls `reload_config_and_clients()` to get new config and clients, then pings ClickHouse to verify connectivity. Only on ping success does it atomically swap all three via `ArcSwap::store()`. Returns `RestartResponse { status: "restarted" }` on success, 500 with error message on failure (old clients remain active -- no partial state).

### Upload Auto-Retention (list.rs, wired in routes.rs + main.rs)
After successful upload, `apply_retention_after_upload()` (defined in `list.rs`) applies local and remote retention per `effective_retention_local/remote()` config. Wired into:
- CLI `main.rs`: upload and create_remote commands (with `manifest_cache: None`)
- API `upload_backup`, `create_remote`, and `post_actions` upload branch (with `manifest_cache: Some(...)` for cache invalidation)

NOT wired into the watch loop (it has its own retention logic) or auto-resume upload path (resume should not trigger retention). Errors are warnings (best-effort, matching watch loop pattern).

### Tables Endpoint (Phase 5)
`GET /api/v1/tables` -- Supports two modes via `TablesParams` query parameters:
- **Live mode** (default): Queries `system.tables` via `ChClient`. If `all=true`, calls `list_all_tables()` (includes system tables); otherwise `list_tables()`. Optional `table` param applies `TableFilter` for glob filtering.
- **Remote mode** (`backup` param set): Downloads manifest from S3 (`{backup_name}/metadata.json`), iterates `manifest.tables`, applies `TableFilter` if `table` param set. Returns table info derived from manifest (engine, total_bytes from sum of part sizes).
- Returns `Vec<TablesResponseEntry>` with fields: `database`, `name`, `engine`, `uuid`, `data_paths`, `total_bytes`.

### Sync Function Handling
Sync functions from `list` module (`list_local`, `delete_local`, `clean_broken_local`) are called via `tokio::task::spawn_blocking` to avoid blocking the async runtime.

### WatchStartRequest (routes.rs)
`WatchStartRequest` is an optional JSON body for `POST /api/v1/watch/start`. Derives `Debug`, `Deserialize`, `Default`. Fields:
- `watch_interval: Option<String>` -- Override watch interval (e.g. "2h", "30m")
- `full_interval: Option<String>` -- Override full backup interval (e.g. "48h")

When either field is `Some`, the handler clones the current config, applies the overrides, validates via `Config::validate()` (returns 400 on failure), and atomically swaps the config via `ArcSwap::store()` before spawning the watch loop. When the body is absent or empty (`{}`), the handler starts watch with the current config unchanged (backward compatible).

### RBAC/Config/Named Collections Request Fields (Phase 4e)
All four operation request types (`CreateRequest`, `RestoreRequest`, `CreateRemoteRequest`, `RestoreRemoteRequest`) include three optional boolean fields added in Phase 4e:
- `rbac: Option<bool>` -- Enable RBAC backup/restore (default: false via `unwrap_or(false)`)
- `configs: Option<bool>` -- Enable config file backup/restore (default: false)
- `named_collections: Option<bool>` -- Enable named collections backup/restore (default: false)

These fields are `Option<bool>` for backward compatibility -- existing API clients that omit these fields get `None`, which defaults to `false`. The `*_backup_always` config overrides still apply on the implementation side.

### ManifestCache Integration (Phase 8)
`ManifestCache` (defined in `list.rs`) is an in-memory TTL-based cache for remote backup summaries (design 8.4). It avoids redundant S3 manifest downloads during server operation.

- **AppState field**: `manifest_cache: Arc<tokio::sync::Mutex<ManifestCache>>` -- shared across all handlers
- **TTL**: Configured via `general.remote_cache_ttl_secs` (default 300 seconds / 5 minutes)
- **Cache usage**: `list_backups()` and `refresh_backup_counts()` call `list::list_remote_cached(&s3, &state.manifest_cache)` instead of `list::list_remote(&s3)`. Cache hit returns stored summaries; miss falls through to S3 and populates cache.
- **Invalidation**: Cache is explicitly invalidated (`manifest_cache.lock().await.invalidate()`) after mutating operations. The `run_operation()` helper handles invalidation automatically when `invalidate_cache=true`. Manual invalidation in `post_actions` for upload/delete/clean paths.
- **Logging**: `ManifestCache: populated, count=N` on cache fill, `ManifestCache: invalidated` on explicit invalidation

### SIGQUIT Handler (Phase 8)
A SIGQUIT signal handler is spawned in `start_server()` (Unix only, gated by `#[cfg(unix)]`). On `kill -QUIT <pid>`, it captures `std::backtrace::Backtrace::force_capture()` and prints the stack dump to stderr with delimiters (`=== SIGQUIT stack dump ===`). The handler runs in a loop and does not terminate the process. Follows the same pattern as the existing SIGHUP handler. The same handler is also registered in `main.rs` for standalone watch mode.

### Tables Pagination (Phase 8)
`TablesParams` now includes `offset: Option<usize>` and `limit: Option<usize>` query parameters. The `tables()` handler applies `.skip(offset).take(limit)` after building the full result set. An `X-Total-Count` response header reports the total count before pagination for client use. When both parameters are omitted, all results are returned (backward compatible).

### List Endpoint Pagination (routes.rs)
`ListParams` includes `offset: Option<usize>`, `limit: Option<usize>`, and `format: Option<String>` query parameters (same pattern as `TablesParams`). The `list_backups()` handler builds the full result set, applies `desc` sort if requested, then applies `.skip(offset).take(limit)` pagination. An `X-Total-Count` response header reports the pre-pagination total count. The return type is `Result<([(HeaderName, HeaderValue); 1], Json<Vec<ListResponse>>), ...>` to include the header. The `format` field is stored for integration table DDL compatibility but the API always returns JSON (axum `Json` wrapper).

### Response Types for Integration Tables
- `ListResponse` matches ALL columns of `system.backup_list` (name, created, location, size, data_size, object_disk_size, metadata_size, rbac_size, config_size, compressed_size, required). All fields are populated from `BackupSummary` via `summary_to_list_response()`: `object_disk_size` from `s.object_disk_size` (sum of S3 object sizes across all parts), `required` from `s.required` (extracted from first `carried:{base}` source in manifest parts), `metadata_size` from `s.metadata_size` (Phase 5), `rbac_size` and `config_size` from `s.rbac_size` and `s.config_size` (Phase 8, computed during `backup::create()`)
- `ActionResponse` matches ALL columns of `system.backup_actions` (command, start, finish, status, error)
- Both derive `Serialize` + `Deserialize` for bidirectional JSON compatibility with ClickHouse URL engine

### Download Handler Hardlink Support (Phase 5)
The download handler (`download_backup`) passes the `hardlink_exists_files` flag from `DownloadRequest` through to `download::download()`. Auto-resume in `state.rs` passes `false` for `hardlink_exists_files` (resume should not hardlink, it re-downloads).

### Public API
- `build_router(state: AppState) -> Router` -- Assembles all routes with unconditional auth middleware
- `start_server(config: Arc<Config>, ch: ChClient, s3: S3Client, watch: bool, config_path: PathBuf) -> Result<()>` -- Full server lifecycle: state creation, router build, optional watch loop spawn, integration tables, auto-resume, listen, graceful shutdown
- `spawn_watch_from_state(state: &mut AppState, config_path: PathBuf, macros: HashMap<String, String>)` -- Spawn watch loop from API context (used by watch_start endpoint)
- `AppState::new(config, ch, s3, config_path) -> Self` -- Create shared state with semaphore, optional metrics, and watch fields initialized to None/default
- `AppState::try_start_op(command) -> Result<(u64, CancellationToken), &str>` -- Start tracked operation
- `AppState::finish_op(id)` / `fail_op(id, error)` / `kill_op(id: Option<u64>) -> bool` -- Operation exit paths
- `run_operation(state, command, op_label, invalidate_cache, closure) -> Result<Json<OperationStarted>, ...>` -- DRY orchestration helper for all route handlers except `post_actions`
- `validate_backup_name(name) -> Result<(), &str>` -- Path traversal validation for backup names
- `scan_resumable_state_files(data_path) -> Vec<ResumableOp>` -- Find interrupted operations
- `auto_resume(state)` -- Spawn resume tasks for found state files
- `Metrics::new() -> Result<Self, prometheus::Error>` -- Create metrics registry with all 14 families registered
- `Metrics::encode() -> Result<String, prometheus::Error>` -- Encode all metrics to Prometheus text exposition format

### Error Handling
- Handler errors return `(StatusCode, Json<ErrorResponse>)` tuples
- 400 Bad Request for invalid inputs (unknown location, empty command, malicious backup names)
- 401 Unauthorized for auth failures
- 404 Not Found for kill with no running operation
- 423 Locked for concurrent operation rejection
- 500 Internal Server Error for restart failures (old clients remain active)
- Integration table DDL failures are warnings, not fatal errors

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
