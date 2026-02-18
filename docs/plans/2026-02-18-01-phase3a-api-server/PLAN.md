# Plan: Phase 3a -- API Server

## Goal

Implement the HTTP API server for chbackup using axum, enabling Kubernetes sidecar operation with all endpoints from design doc section 9. This includes request handling, action log, authentication, TLS, operation serialization, kill support, auto-resume on restart, and ClickHouse integration tables.

## Architecture Overview

The server is a new `src/server/` module that:
1. Builds an axum `Router` with all API endpoints
2. Shares `AppState` (config, clients, action log, operation management) across handlers via `State`
3. Each handler delegates to existing command functions (`backup::create`, `upload::upload`, etc.)
4. Background operations run in `tokio::spawn` tasks, tracked in an `ActionLog` ring buffer
5. A `CancellationToken` per operation enables `POST /kill`
6. Optional Basic auth middleware gates all endpoints
7. Optional TLS via `axum-server` with rustls

**File layout:**
```
src/server/
  mod.rs          -- build_router(), start_server(), AppState, re-exports
  routes.rs       -- All endpoint handler functions
  actions.rs      -- ActionLog ring buffer + ActionEntry types
  auth.rs         -- Basic auth middleware
  state.rs        -- Operation state management (serialization, cancellation)
```

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Config**: Loaded in `main.rs`, passed to `start_server()` as `Arc<Config>`. Server owns `Arc<Config>`.
- **ChClient**: Created once at server startup, stored in `AppState`. Shared via `Clone` (it implements Clone).
- **S3Client**: Created once at server startup, stored in `AppState`. Shared via `Clone`.
- **ActionLog**: Created at server startup, stored in `AppState` behind `Arc<Mutex<ActionLog>>`. Modified by handlers.
- **CancellationToken**: Created per operation, stored in `AppState`'s current operation tracking. Cancelled by `POST /kill`.
- **PidLock**: Used per operation (backup-scoped), not stored in AppState. Each background task acquires/releases its own lock.
- **Integration tables**: Created at startup via `ChClient::execute_ddl()`, dropped at shutdown.

### What This Plan CANNOT Do
- Watch mode (Phase 3d) -- endpoints return "not implemented" stub
- Prometheus metrics (Phase 3b) -- `/metrics` returns "not implemented" stub
- Retention/GC (Phase 3c) -- `POST /api/v1/clean` returns "not implemented" stub
- Table remap / `--as` flag (Phase 4a) -- restore_remote params are limited
- RBAC/config backup (Phase 4e) -- backup params ignore rbac/configs flags
- `POST /api/v1/tables` -- `tables` command not implemented, returns stub

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| axum 0.7 API surface unfamiliar | GREEN | Well-documented, widely used. Plan verified against official docs. |
| Operation serialization correctness | YELLOW | Careful Mutex design. Test concurrent request rejection (409). |
| Integration table DDL fails on ClickHouse | YELLOW | Use `IF NOT EXISTS` / `IF EXISTS`. Startup logs warning and continues. |
| CancellationToken not checked by command functions | YELLOW | Phase 3a passes token but existing commands ignore it. Kill cancels the spawn wrapper, not individual parts. Future phases can thread token deeper. |
| TLS certificate file paths wrong at runtime | GREEN | Server logs error and exits with clear message. |
| Auto-resume scans wrong directory | GREEN | Uses `config.clickhouse.data_path` + "backup/" -- same as CLI commands. |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Starting API server on {listen}` | yes | Server bind confirmation |
| `API authentication enabled` | conditional | Only when username/password set |
| `TLS enabled` | conditional | Only when secure=true |
| `Integration tables created` | conditional | Only when create_integration_tables=true |
| `Auto-resume: found {n} resumable operations` | conditional | Only when state files exist |
| `Action started: {command}` | yes | Every operation start |
| `Action completed: {command}` | yes | Every operation success |
| `Action failed: {command}: {error}` | conditional | On operation error |
| `Operation killed` | conditional | On POST /kill |
| `409 Conflict: operation already in progress` | conditional | When allow_parallel=false and op running |
| `ERROR:` | no (forbidden) | Should NOT appear during normal startup |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Watch mode endpoints | Phase 3d | Endpoints return stub, wired in Phase 3d |
| Prometheus metrics | Phase 3b | `/metrics` returns stub |
| Retention/GC via API | Phase 3c | `POST /api/v1/clean` returns stub |
| Table remap (`--as`) | Phase 4a | Not supported in restore params |
| RBAC/config backup flags | Phase 4e | Accepted but ignored |
| `POST /api/v1/tables` | Phase 4f | Returns stub |
| Config hot-reload (SIGHUP) | Phase 3d | `POST /reload` returns stub |
| `POST /restart` full implementation | Phase 3d | Returns stub (restart requires watch loop coordination) |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: Add dependencies to Cargo.toml + add Serialize to BackupSummary
  - Task 2: ActionLog ring buffer and ActionEntry types (src/server/actions.rs)
  - Task 3: AppState + operation management (src/server/state.rs)
  - Task 4: Basic auth middleware (src/server/auth.rs)

Group B (Routes -- Sequential, depends on Group A):
  - Task 5: Health, version, status, actions endpoints (read-only routes)
  - Task 6: Backup operation endpoints (create, upload, download, restore, etc.)
  - Task 7: Delete, clean, kill, and stub endpoints

Group C (Server Assembly -- Sequential, depends on Group B):
  - Task 8: Router assembly + server startup (src/server/mod.rs)
  - Task 9: Integration tables (ChClient methods + startup/shutdown DDL)
  - Task 10: Auto-resume on restart
  - Task 11: Wire Command::Server in main.rs + add pub mod server to lib.rs

Group D (Documentation -- depends on Group C):
  - Task 12: Create CLAUDE.md for src/server, update src/clickhouse/CLAUDE.md
```

## Tasks

### Task 1: Add dependencies and derive Serialize on BackupSummary

**TDD Steps:**
1. Write failing test: `test_phase3a_deps_available` in `src/lib.rs` tests that verifies `axum`, `tower_http`, `base64`, `tokio_util::sync::CancellationToken` are importable
2. Add dependencies to `Cargo.toml`: `axum = "0.7"`, `tower-http = { version = "0.6", features = ["auth"] }`, `base64 = "0.22"`, `axum-server = { version = "0.7", features = ["tls-rustls"] }`, `tower = "0.5"`, `http = "1"`
3. Add `#[derive(serde::Serialize)]` to `BackupSummary` in `src/list.rs` (line 24, change `#[derive(Debug, Clone)]` to `#[derive(Debug, Clone, serde::Serialize)]`)
4. Add `use serde::Serialize;` import if not already present in list.rs scope
5. Verify `cargo check` passes with zero warnings
6. Write test: `test_backup_summary_serializable` that serializes a `BackupSummary` to JSON

**Files:**
- `Cargo.toml`
- `src/list.rs`
- `src/lib.rs` (test)

**Acceptance:** F001

**Implementation Notes:**
- Check exact tower-http version compatibility with axum 0.7 (tower-http 0.6.x is compatible)
- `tokio-util` already has `codec` feature; CancellationToken is in base crate, no feature needed
- `serde` import already exists in `list.rs` via `crate::manifest::BackupManifest` path but `Serialize` may not be in scope; use fully qualified derive `serde::Serialize`

---

### Task 2: ActionLog ring buffer and ActionEntry types

**TDD Steps:**
1. Create `src/server/actions.rs`
2. Define `ActionStatus` enum: `Running`, `Completed`, `Failed(String)`, `Killed`
3. Define `ActionEntry` struct with fields: `id: u64`, `command: String`, `start: DateTime<Utc>`, `finish: Option<DateTime<Utc>>`, `status: ActionStatus`, `error: Option<String>`
4. Both `ActionStatus` and `ActionEntry` derive `Debug, Clone, Serialize`
5. Define `ActionLog` struct with fields: `entries: VecDeque<ActionEntry>`, `capacity: usize`, `next_id: u64`
6. Implement `ActionLog::new(capacity: usize) -> Self`
7. Implement `ActionLog::start(&mut self, command: String) -> u64` -- pushes new entry with `Running` status, pops front if over capacity, returns id
8. Implement `ActionLog::finish(&mut self, id: u64)` -- sets `finish` time and `Completed` status
9. Implement `ActionLog::fail(&mut self, id: u64, error: String)` -- sets `finish` time and `Failed` status
10. Implement `ActionLog::kill(&mut self, id: u64)` -- sets `finish` time and `Killed` status
11. Implement `ActionLog::entries(&self) -> &VecDeque<ActionEntry>` -- for read access
12. Implement `ActionLog::running(&self) -> Option<&ActionEntry>` -- find entry with `Running` status
13. Write unit test: `test_action_log_start_finish` -- start action, verify Running, finish, verify Completed
14. Write unit test: `test_action_log_capacity` -- add entries beyond capacity, verify oldest dropped
15. Write unit test: `test_action_log_fail` -- start then fail, verify error message stored
16. Write unit test: `test_action_log_running` -- start action, verify `running()` returns it, finish, verify `running()` returns None
17. Verify `cargo test` passes

**Files:**
- `src/server/actions.rs` (new)
- `src/server/mod.rs` (new, initially just `pub mod actions;`)

**Acceptance:** F002

**Implementation Notes:**
- Capacity default: 100 entries (configurable later)
- Use `chrono::Utc::now()` for timestamps (chrono already a dependency)
- `ActionEntry` also needs `Serialize` for the `/api/v1/actions` JSON response
- `ActionStatus` needs `Serialize` -- use `#[serde(rename_all = "snake_case")]` for JSON output
- Do NOT use `serde::Serialize` for `ActionLog` itself (internal type)

---

### Task 3: AppState and operation management

**TDD Steps:**
1. Create `src/server/state.rs`
2. Define `AppState` struct (must be `Clone` for axum):
   ```rust
   #[derive(Clone)]
   pub struct AppState {
       pub config: Arc<Config>,
       pub ch: ChClient,
       pub s3: S3Client,
       pub action_log: Arc<Mutex<ActionLog>>,
       pub current_op: Arc<Mutex<Option<RunningOp>>>,
       pub op_semaphore: Arc<Semaphore>,
   }
   ```
3. Define `RunningOp` struct: `id: u64`, `command: String`, `cancel_token: CancellationToken`
4. Implement `AppState::new(config: Arc<Config>, ch: ChClient, s3: S3Client) -> Self`
   - Creates `ActionLog::new(100)`
   - Creates `op_semaphore` with 1 permit (for allow_parallel=false) or `usize::MAX` (for allow_parallel=true) based on `config.api.allow_parallel`
5. Implement `AppState::try_start_op(&self, command: &str) -> Result<(u64, CancellationToken), &'static str>`
   - Try acquire semaphore permit (non-blocking)
   - If acquired: create RunningOp, store in current_op, log start in action_log, return (id, token)
   - If not acquired: return Err("operation already in progress")
6. Implement `AppState::finish_op(&self, id: u64)`
   - Lock action_log, call finish(id)
   - Lock current_op, clear it if matching id
7. Implement `AppState::fail_op(&self, id: u64, error: String)`
   - Lock action_log, call fail(id, error)
   - Lock current_op, clear it if matching id
8. Implement `AppState::kill_current(&self) -> bool`
   - Lock current_op, if Some: cancel token, set status to Killed, clear, return true
   - If None: return false
9. Write unit test: `test_app_state_start_finish_op` -- start op, verify running, finish, verify done
10. Write unit test: `test_app_state_sequential_ops_blocked` -- with allow_parallel=false, start op, try start another, expect Err
11. Write unit test: `test_app_state_kill` -- start op, kill, verify token cancelled
12. Verify `cargo test` passes

**Files:**
- `src/server/state.rs` (new)
- `src/server/mod.rs` (add `pub mod state;`)

**Acceptance:** F003

**Implementation Notes:**
- Use `tokio::sync::Mutex` for `action_log` and `current_op` since they are accessed from async handlers
- Use `tokio::sync::Semaphore` with `try_acquire_owned()` for non-blocking permit check
- The semaphore permit must be held for the duration of the operation (store `OwnedSemaphorePermit` in `RunningOp`)
- `CancellationToken` from `tokio_util::sync` (already available, no extra feature needed)
- `AppState` must be `Clone` -- all inner fields are `Arc`-wrapped or `Clone`
- `ChClient` is `Clone` (verified in symbols.md)
- `S3Client` is `Clone` (verified in symbols.md)

---

### Task 4: Basic auth middleware

**TDD Steps:**
1. Create `src/server/auth.rs`
2. Implement `auth_middleware` as an axum middleware function:
   ```rust
   pub async fn auth_middleware(
       State(state): State<AppState>,
       request: axum::http::Request<axum::body::Body>,
       next: axum::middleware::Next,
   ) -> axum::response::Response
   ```
3. Logic:
   - If `config.api.username` is empty AND `config.api.password` is empty: pass through (no auth required)
   - Extract `Authorization` header
   - If missing: return 401 with `WWW-Authenticate: Basic` header
   - Decode Base64 value after "Basic " prefix using `base64::engine::general_purpose::STANDARD`
   - Split on `:` to get username:password
   - Compare against `config.api.username` and `config.api.password`
   - If match: call `next.run(request).await`
   - If mismatch: return 401
4. Write unit test: `test_auth_no_config` -- empty username/password, request passes through
5. Write unit test: `test_auth_valid_credentials` -- correct Basic auth header, request passes
6. Write unit test: `test_auth_invalid_credentials` -- wrong credentials, returns 401
7. Write unit test: `test_auth_missing_header` -- no Authorization header, returns 401
8. Verify `cargo test` passes

**Files:**
- `src/server/auth.rs` (new)
- `src/server/mod.rs` (add `pub mod auth;`)

**Acceptance:** F004

**Implementation Notes:**
- Use `axum::middleware::from_fn_with_state` to attach the middleware to the router
- Base64 decode: `use base64::{Engine as _, engine::general_purpose::STANDARD};`
- The middleware wraps the ENTIRE router (all endpoints require auth when configured)
- `/health` might be excluded from auth in production, but design doc says "all endpoints" -- follow design doc
- For tests, use `axum::test_helpers` or construct mock requests manually with `axum::body::Body` and tower's `ServiceExt`

---

### Task 5: Read-only route handlers (health, version, status, actions)

**TDD Steps:**
1. Create `src/server/routes.rs`
2. Implement `GET /health` handler:
   ```rust
   pub async fn health() -> &'static str { "OK" }
   ```
3. Implement `GET /api/v1/version` handler:
   ```rust
   pub async fn version(State(state): State<AppState>) -> Json<VersionResponse>
   ```
   - Returns `{ "version": env!("CARGO_PKG_VERSION"), "clickhouse_version": ch.get_version() }`
   - If CH version query fails, return "unknown" for clickhouse_version
4. Define `VersionResponse` struct: `version: String`, `clickhouse_version: String` (derive `Serialize`)
5. Implement `GET /api/v1/status` handler:
   ```rust
   pub async fn status(State(state): State<AppState>) -> Json<StatusResponse>
   ```
   - Returns current running operation info (command, start time) or "idle"
6. Define `StatusResponse` struct: `status: String`, `command: Option<String>`, `start: Option<String>` (derive `Serialize`)
7. Implement `GET /api/v1/actions` handler:
   ```rust
   pub async fn get_actions(State(state): State<AppState>) -> Json<Vec<ActionResponse>>
   ```
   - Returns the action log entries serialized as JSON
8. Define `ActionResponse` struct matching the integration table schema: `command: String`, `start: String`, `finish: String`, `status: String`, `error: String` (derive `Serialize, Deserialize`)
9. Implement `POST /api/v1/actions` handler (integration table INSERT dispatch):
   ```rust
   pub async fn post_actions(
       State(state): State<AppState>,
       Json(body): Json<Vec<ActionRequest>>,
   ) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)>
   ```
   - Accepts JSONEachRow from ClickHouse URL engine INSERT (array of `{"command": "..."}`)
   - Parses first `command` string: split on whitespace to get operation name and arguments
   - Dispatches to the appropriate operation handler (create, upload, download, restore, create_remote, delete, clean_broken, etc.)
   - Returns 200 on successful dispatch
10. Define `ActionRequest` struct: `command: String` (derive `Deserialize`)
11. Implement `GET /api/v1/list` handler:
   ```rust
   pub async fn list_backups(
       State(state): State<AppState>,
       Query(params): Query<ListParams>,
   ) -> Result<Json<Vec<ListResponse>>, StatusCode>
   ```
   - Calls `list::list_local` and/or `list::list_remote` based on `location` query param
   - Converts `BackupSummary` to `ListResponse`, filling `location` ("local"/"remote"), defaulting `data_size`/`object_disk_size`/`rbac_size`/`config_size`/`required` to 0/empty (computed from manifest where possible)
12. Define `ListParams` struct: `location: Option<String>` (derive `Deserialize`)
13. Define `ListResponse` struct matching ALL columns of the `system.backup_list` integration table:
   `name: String`, `created: String`, `location: String`, `size: u64`, `data_size: u64`, `object_disk_size: u64`, `metadata_size: u64`, `rbac_size: u64`, `config_size: u64`, `compressed_size: u64`, `required: String` (derive `Serialize`)
   - `data_size`: for now, same as `size` (total uncompressed). Phase 3+ can split local vs S3 disk.
   - `object_disk_size`: 0 for now (requires manifest disk_types analysis).
   - `rbac_size`, `config_size`: 0 (not implemented until Phase 4e).
   - `required`: empty string (no dependency chain tracking yet).
   - `metadata_size`: from manifest `metadata_size` field.
14. Write unit test: `test_health_returns_ok`
15. Write unit test: `test_actions_empty_log`
16. Write unit test: `test_list_response_all_columns` -- verify ListResponse has all integration table columns
17. Write unit test: `test_post_actions_dispatch` -- verify POST /api/v1/actions parses command string
18. Verify `cargo test` passes

**Files:**
- `src/server/routes.rs` (new)
- `src/server/mod.rs` (add `pub mod routes;`)

**Acceptance:** F005

**Implementation Notes:**
- The `/api/v1/actions` GET response must match the `system.backup_actions` table schema (columns: command, start, finish, status, error) for ClickHouse URL engine SELECT compatibility
- The `/api/v1/actions` POST handler must accept JSONEachRow from ClickHouse URL engine INSERT (array of `{"command": "..."}`) and dispatch operations. This enables `INSERT INTO system.backup_actions(command) VALUES ('create_remote daily_backup')` from clickhouse-client.
- The `/api/v1/list` response must match ALL columns of the `system.backup_list` table schema for URL engine compatibility: name, created, location, size, data_size, object_disk_size, metadata_size, rbac_size, config_size, compressed_size, required
- Use `axum::extract::Query` for GET query params
- Use `axum::Json` for JSON responses
- DateTime formatting: use ISO 8601 format via `chrono::DateTime::to_rfc3339()`
- All response structs in routes.rs derive `Serialize`

---

### Task 6: Backup operation endpoints

**TDD Steps:**
1. Implement `POST /api/v1/create` handler:
   - Parse `CreateRequest` from JSON body: `tables: Option<String>`, `diff_from: Option<String>`, `schema: Option<bool>`, `partitions: Option<String>`, `backup_name: Option<String>`, `skip_check_parts_columns: Option<bool>`
   - Call `state.try_start_op("create")` -- return 409 if blocked
   - Spawn background task that calls `backup::create(...)` with params from request
   - On success: `state.finish_op(id)`
   - On error: `state.fail_op(id, error.to_string())`
   - Return 200 with action id immediately (async operation)
2. Define `CreateRequest` struct (derive `Deserialize, Default`)
3. Implement `POST /api/v1/upload/{name}` handler:
   - Parse `UploadRequest`: `delete_local: Option<bool>`, `diff_from_remote: Option<String>`
   - Extract `name` from path
   - Spawn background task calling `upload::upload(...)`
4. Define `UploadRequest` struct (derive `Deserialize, Default`)
5. Implement `POST /api/v1/download/{name}` handler:
   - Parse `DownloadRequest`: `hardlink_exists_files: Option<bool>` (accepted but logged as not-yet-implemented, per design doc §9 API spec)
   - Spawn background task calling `download::download(...)`
6. Define `DownloadRequest` struct (derive `Deserialize, Default`)
7. Implement `POST /api/v1/restore/{name}` handler:
   - Parse `RestoreRequest`: `tables: Option<String>`, `schema: Option<bool>`, `data_only: Option<bool>`, `database_mapping: Option<String>`, `rm: Option<bool>`
   - `database_mapping` and `rm` are accepted but logged as not-yet-implemented (Phase 4a/4d)
   - Spawn background task calling `restore::restore(...)`
8. Define `RestoreRequest` struct (derive `Deserialize, Default`)
9. Implement `POST /api/v1/create_remote` handler:
   - Parse `CreateRemoteRequest`: `tables: Option<String>`, `diff_from_remote: Option<String>`, `backup_name: Option<String>`, `delete_source: Option<bool>`, `skip_check_parts_columns: Option<bool>`
   - Spawn background task that calls `backup::create(...)` then `upload::upload(...)`
10. Implement `POST /api/v1/restore_remote/{name}` handler:
    - Spawn background task that calls `download::download(...)` then `restore::restore(...)`
    - Note: This is a NEW implementation (main.rs restore_remote is still a stub). Chains existing `download::download()` + `restore::restore()`.
11. Define `OperationStarted` response struct: `id: u64`, `status: String` (derive `Serialize`)
12. Each handler follows the pattern:
    ```rust
    async fn handler(State(state): State<AppState>, ...) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
        let (id, token) = state.try_start_op("command")
            .map_err(|e| (StatusCode::CONFLICT, Json(ErrorResponse { error: e.to_string() })))?;
        let state_clone = state.clone();
        tokio::spawn(async move {
            let result = do_operation(&state_clone, token).await;
            match result {
                Ok(_) => state_clone.finish_op(id),
                Err(e) => state_clone.fail_op(id, e.to_string()),
            }
        });
        Ok(Json(OperationStarted { id, status: "started".into() }))
    }
    ```
13. Write unit test: `test_create_request_deserialization` -- verify JSON parsing of CreateRequest
14. Write unit test: `test_operation_started_response` -- verify OperationStarted serializes correctly
15. Write unit test: `test_restore_request_accepts_unimplemented_fields` -- verify database_mapping and rm are accepted in JSON
16. Verify `cargo test` passes

**Files:**
- `src/server/routes.rs` (extend)

**Acceptance:** F006

**Implementation Notes:**
- Use `axum::extract::Path` for URL path parameters (e.g., `Path(name): Path<String>`)
- Use `axum::extract::Json` for request bodies (with `Option<Json<T>>` to handle empty bodies)
- `backup_name` auto-generation: use `resolve_backup_name()` pattern from main.rs (UTC timestamp format)
- `resume` is always true for server-mode operations (design: auto-resume)
- `effective_resume = config.general.use_resumable_state` (no CLI flag in API)
- The `CancellationToken` is passed to the spawned task but existing command functions do not use it yet. The token cancels the outer select if needed.
- All operation handlers must handle the case where `ChClient` or `S3Client` initialization fails (these are created at startup, so failure here is unlikely but must be handled)
- Per design doc: `backup_dir` for upload is `config.clickhouse.data_path + "/backup/" + name`
- `DownloadRequest.hardlink_exists_files`: accepted per design doc §9 API spec but logged as "not yet implemented" (no existing support in download::download)
- `RestoreRequest.database_mapping` and `RestoreRequest.rm`: accepted per design doc §9 API spec but logged as "not yet implemented" (Phase 4a/4d)
- `restore_remote` is a NEW server-side implementation: chains `download::download()` + `restore::restore()`. The CLI `Command::RestoreRemote` in main.rs is still a stub — the server implements it first.

---

### Task 7: Delete, clean, kill, and stub endpoints

**TDD Steps:**
1. Implement `DELETE /api/v1/delete/{where}/{name}` handler:
   - Extract `location` ("local" or "remote") and `name` from path
   - Map to `list::Location` enum
   - Call `list::delete(...)` in background task
2. Implement `POST /api/v1/clean/remote_broken` handler:
   - Call `list::clean_broken_remote(...)` in background task
3. Implement `POST /api/v1/clean/local_broken` handler:
   - Call `list::clean_broken_local(...)` in background task (sync via spawn_blocking since it is `fn` not `async fn`)
4. Implement `POST /api/v1/kill` handler:
   - Call `state.kill_current()` -- if true return 200 "killed", if false return 404 "no running operation"
5. Implement stub endpoints (return 501 Not Implemented):
   - `POST /api/v1/clean` -- "not implemented (Phase 3c)"
   - `POST /api/v1/reload` -- "not implemented (Phase 3d)"
   - `POST /api/v1/restart` -- "not implemented (Phase 3d)"
   - `GET /api/v1/tables` -- "not implemented (Phase 4f)"
   - `POST /api/v1/watch/start` -- "not implemented (Phase 3d)"
   - `POST /api/v1/watch/stop` -- "not implemented (Phase 3d)"
   - `GET /api/v1/watch/status` -- "not implemented (Phase 3d)"
   - `GET /metrics` -- "not implemented (Phase 3b)"
6. Define `ErrorResponse` struct: `error: String` (derive `Serialize`)
7. Write unit test: `test_error_response_serialization`
8. Write unit test: `test_delete_path_parsing` -- verify "local"/"remote" string maps correctly to Location
9. Verify `cargo test` passes

**Files:**
- `src/server/routes.rs` (extend)

**Acceptance:** F007

**Implementation Notes:**
- `list::clean_broken_local(data_path)` is a sync function (`fn`, not `async fn`). Must be called via `tokio::task::spawn_blocking` or converted. Since it does filesystem I/O this is appropriate.
- `list::delete_local(data_path, name)` is also sync. Same treatment.
- `list::delete_remote(s3, name)` is async. Fine to use directly.
- For delete: design doc uses `DELETE /api/v1/delete/{where}/{name}` where `{where}` is "local" or "remote"
- Kill should also update the action log status via `state.action_log`

---

### Task 8: Router assembly and server startup

**TDD Steps:**
1. Implement `build_router(state: AppState) -> Router` in `src/server/mod.rs`:
   - Attach all routes from routes.rs (both GET and POST for `/api/v1/actions`)
   - Conditionally add auth middleware if username/password configured
   - Return the complete Router
2. Implement `start_server(config: Arc<Config>, ch: ChClient, s3: S3Client) -> Result<()>`:
   - Create `AppState::new(config.clone(), ch, s3)`
   - Build router via `build_router(state)`
   - Parse `config.api.listen` as `SocketAddr`
   - If `config.api.secure`:
     - Load certificate and key files
     - Start with `axum_server::bind_rustls(...).serve(...)`
   - Else:
     - Start with `axum::serve(listener, router).await`
   - Log "Starting API server on {listen}"
3. Handle graceful shutdown: listen for Ctrl+C signal, drop integration tables on shutdown
4. Write unit test: `test_build_router_creates_valid_router` -- construct AppState with test config, build router, verify it does not panic
5. Write integration-style test: `test_health_endpoint` -- use `axum::test::TestClient` or tower's `ServiceExt::oneshot` to send GET /health and verify 200 "OK"
6. Verify `cargo check` passes

**Files:**
- `src/server/mod.rs` (extend with build_router, start_server, re-exports)

**Acceptance:** F008

**Implementation Notes:**
- `axum::serve` in 0.7 takes a `TcpListener` and a `Router`. Use `tokio::net::TcpListener::bind(&addr).await?`
- For TLS: `axum_server` 0.7 provides `axum_server::bind_rustls(addr, tls_config).serve(router.into_make_service())`
- RustlsConfig: `axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path).await?`
- Auth middleware: `router.layer(axum::middleware::from_fn_with_state(state.clone(), auth::auth_middleware))`
- Graceful shutdown pattern:
  ```rust
  let shutdown = tokio::signal::ctrl_c();
  axum::serve(listener, router)
      .with_graceful_shutdown(async { shutdown.await.ok(); })
      .await?;
  ```
- After shutdown signal: drop integration tables, log "Server stopped"

---

### Task 9: Integration tables (ChClient methods + startup/shutdown DDL)

**TDD Steps:**
1. Add `create_integration_tables(&self, api_host: &str, api_port: &str) -> Result<()>` method to `ChClient` in `src/clickhouse/client.rs`:
   - Execute CREATE TABLE IF NOT EXISTS for `system.backup_list` URL engine table
   - Execute CREATE TABLE IF NOT EXISTS for `system.backup_actions` URL engine table
   - URL format: `http://{api_host}:{api_port}/api/v1/list` and `.../api/v1/actions`
2. Add `drop_integration_tables(&self) -> Result<()>` method to `ChClient`:
   - Execute DROP TABLE IF EXISTS for both tables
3. Wire into server startup: if `config.api.create_integration_tables` is true, call `ch.create_integration_tables(...)` after server binds
4. Wire into server shutdown: call `ch.drop_integration_tables()` during graceful shutdown
5. Parse `integration_tables_host`: if empty, use "localhost". Parse listen address to extract port.
6. Write unit test: `test_integration_table_ddl_generation` -- verify the SQL strings are correct (test the DDL string builder, not actual execution)
7. Verify `cargo check` passes

**Files:**
- `src/clickhouse/client.rs` (add 2 new methods)
- `src/server/mod.rs` (wire startup/shutdown calls)

**Acceptance:** F009

**Implementation Notes:**
- The SQL must match the design doc exactly (section 9.1):
  ```sql
  CREATE TABLE IF NOT EXISTS system.backup_list (
      name String,
      created DateTime,
      location String,
      size UInt64,
      data_size UInt64,
      object_disk_size UInt64,
      metadata_size UInt64,
      rbac_size UInt64,
      config_size UInt64,
      compressed_size UInt64,
      required String
  ) ENGINE = URL('http://{host}:{port}/api/v1/list', 'JSONEachRow');
  ```
- Parse api.listen to extract host and port. `api.listen` format is "host:port" (e.g., "localhost:7171")
- If `integration_tables_host` is non-empty, use it as the host component of the URL
- Integration table creation failures should be logged as warnings but not prevent server startup
- `execute_ddl()` already exists on ChClient (verified in knowledge_graph.json)

---

### Task 10: Auto-resume on restart

**TDD Steps:**
1. Implement `scan_resumable_state_files(data_path: &str) -> Vec<ResumableOp>` in `src/server/state.rs`:
   - Walk `{data_path}/backup/` directory
   - For each subdirectory, check for `upload.state.json`, `download.state.json`, `restore.state.json`
   - Return list of `ResumableOp { backup_name: String, op_type: String }` for each found state file
2. Define `ResumableOp` struct: `backup_name: String`, `op_type: String` (upload/download/restore)
3. Implement `auto_resume(state: &AppState)` function:
   - If `config.api.complete_resumable_after_restart` is false, return early
   - Call `scan_resumable_state_files`
   - For each found state file, spawn the corresponding operation with `resume=true`
   - Log "Auto-resume: found {n} resumable operations" (or "no resumable operations found")
4. Wire `auto_resume` into `start_server()` after server is ready
5. Write unit test: `test_scan_resumable_empty_dir` -- empty dir returns empty vec
6. Write unit test: `test_scan_resumable_finds_state_files` -- create temp dir with state files, verify detection
7. Verify `cargo test` passes

**Files:**
- `src/server/state.rs` (extend)
- `src/server/mod.rs` (wire auto_resume call)

**Acceptance:** F010

**Implementation Notes:**
- Use `std::fs::read_dir` to walk the backup directory (sync, via spawn_blocking if needed, but scan is fast)
- State file names are: `upload.state.json`, `download.state.json`, `restore.state.json` (per resume.rs convention)
- Note: Design doc §9 only mentions `upload|download` for auto-resume, but including `restore` is a safe extension since RestoreState exists in resume.rs and interrupted restores benefit from auto-resume equally
- Auto-resume fires asynchronously (spawned tasks) so server starts serving immediately
- If auto-resume spawns an operation, it goes through `try_start_op()` which respects `allow_parallel`
- If `allow_parallel=false` and multiple state files found, operations queue sequentially (first one starts, rest wait or are deferred)
- For Phase 3a, keep it simple: spawn each resumable operation sequentially with a small delay between them

---

### Task 11: Wire Command::Server in main.rs + pub mod server

**TDD Steps:**
1. Add `pub mod server;` to `src/lib.rs`
2. Replace the stub in `src/main.rs` `Command::Server` arm:
   ```rust
   Command::Server { watch } => {
       if watch {
           warn!("--watch flag is not yet implemented (Phase 3d), running server without watch");
       }
       let ch = ChClient::new(&config.clickhouse)?;
       let s3 = S3Client::new(&config.s3).await?;
       chbackup::server::start_server(Arc::new(config), ch, s3).await?;
   }
   ```
3. Add `use std::sync::Arc;` import to main.rs (if not present)
4. Verify `cargo check` passes with zero warnings
5. Verify `cargo build --release` completes (may take longer with new deps)

**Files:**
- `src/lib.rs` (add pub mod server)
- `src/main.rs` (wire server startup)

**Acceptance:** F011

**Implementation Notes:**
- No PidLock change needed: `lock_for_command("server", None)` already returns `LockScope::None` (lock.rs:126 catch-all), so no lock is acquired for the server command.
- `Arc::new(config)` wraps the config since `start_server` takes `Arc<Config>`.

---

### Task 12: Create CLAUDE.md for src/server, update src/clickhouse/CLAUDE.md

**TDD Steps:**
1. Create `src/server/CLAUDE.md` using the template
2. Auto-generate directory tree: `tree -L 2 src/server --noreport`
3. Document key patterns: AppState sharing, route handler delegation, ActionLog ring buffer, auth middleware
4. Update `src/clickhouse/CLAUDE.md` to add integration table methods documentation
5. Validate both files have required sections: Parent Context, Directory Structure, Key Patterns, Parent Rules

**Files:**
- `src/server/CLAUDE.md` (new)
- `src/clickhouse/CLAUDE.md` (update)

**Acceptance:** FDOC

**Implementation Notes:**
- `src/server/CLAUDE.md` documents the new module: purpose, file layout, AppState fields, route handler pattern, operation lifecycle
- `src/clickhouse/CLAUDE.md` adds two bullet points to the Public API section: `create_integration_tables(host, port)` and `drop_integration_tables()`
- Preserve all existing content in clickhouse/CLAUDE.md

---

## Notes

### Phase 4.5: Interface Skeleton Simulation

Skip Reason: This plan introduces a NEW module (`src/server/`) using NEW external crates (`axum`, `tower-http`, `base64`, `axum-server`) that are not yet in Cargo.toml. A plan_stub.rs cannot compile until dependencies are added (Task 1). The dependency addition itself is straightforward and verified against crates.io compatibility.

However, Task 1 explicitly includes a compile-time verification test (`test_phase3a_deps_available`) that serves the same purpose as Phase 4.5 -- it verifies the dependencies are correctly wired.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (cross-task types) | PASS | All types defined in earlier tasks before use in later tasks. AppState (Task 3) used by all route handlers (Tasks 5-8). ActionLog (Task 2) used by AppState (Task 3). |
| RC-016 (struct field completeness) | PASS | AppState fields cover all handler needs: config, ch, s3, action_log, current_op, op_semaphore. ActionEntry fields match /actions response schema. |
| RC-017 (state field declarations) | PASS | All self.X references trace to AppState fields defined in Task 3 or existing codebase types. |
| RC-018 (TDD sequencing) | PASS | Task 2 defines ActionLog before Task 3 uses it. Task 3 defines AppState before Tasks 5-8 use it. Task 4 defines auth before Task 8 attaches it. |
| RC-006 (verified APIs) | PASS | All existing function signatures verified in knowledge_graph.json. New axum APIs are from well-documented public crate. |
| RC-008 (field ordering) | PASS | No tuple types assumed. All struct fields explicitly listed. |
| RC-019 (existing patterns) | PASS | Operation delegation follows exact pattern from main.rs match arms. |
| RC-011 (state flags exit paths) | PASS | RunningOp cleared on finish (success), fail (error), and kill (cancellation) -- all three exit paths handled in Task 3. |
