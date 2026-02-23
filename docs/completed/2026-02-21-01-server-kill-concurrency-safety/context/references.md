# Symbol and Reference Analysis

## Key Symbols and Their References

### AppState::try_start_op (state.rs:150)

**Signature:** `pub async fn try_start_op(&self, command: &str) -> Result<(u64, CancellationToken), &'static str>`

**References (15 call sites across 2 files):**

| File | Line | Handler |
|------|------|---------|
| routes.rs | 243 | `post_actions` |
| routes.rs | 601 | `create_backup` -- `let (id, _token) = ...` |
| routes.rs | 694 | `upload_backup` -- `let (id, _token) = ...` |
| routes.rs | 777 | `download_backup` -- `let (id, _token) = ...` |
| routes.rs | 844 | `restore_backup` -- `let (id, _token) = ...` |
| routes.rs | 952 | `create_remote` -- `let (id, _token) = ...` |
| routes.rs | 1082 | `restore_remote` -- `let (id, _token) = ...` |
| routes.rs | 1227 | `delete_backup` -- `let (id, _token) = ...` |
| routes.rs | 1296 | `clean_remote_broken` -- `let (id, _token) = ...` |
| routes.rs | 1357 | `clean_local_broken` -- `let (id, _token) = ...` |
| routes.rs | 1433 | `clean` -- `let (id, _token) = ...` |
| state.rs | 306 | `auto_resume` (upload) -- `let (id, _token) = ...` |
| state.rs | 351 | `auto_resume` (download) -- `let (id, _token) = ...` |
| state.rs | 390 | `auto_resume` (restore) -- `let (id, _token) = ...` |

**Critical finding:** ALL 14 call sites (11 routes + 3 auto-resume) bind the CancellationToken to `_token` (prefixed underscore = intentionally unused). The token is NEVER passed to the spawned task or the operation function.

### AppState::kill_current (state.rs:210)

**Signature:** `pub async fn kill_current(&self) -> bool`

**References (2):**
- routes.rs:1417 -- `kill_op` handler
- state.rs:210 -- definition

**Analysis:** The `kill_op` handler calls `state.kill_current()` which cancels the token stored in `RunningOp.cancel_token`. However, since no operation function checks `is_cancelled()` and the token was never passed to the spawned task, the cancellation has NO EFFECT on the running operation. The operation is marked as "Killed" in ActionLog, the semaphore permit is dropped (allowing next op), but the actual task continues running to completion in the background.

### AppState::current_op (state.rs:70)

**Type:** `Arc<Mutex<Option<RunningOp>>>`

**References (8):**
- state.rs:135 -- initialization (`Arc::new(Mutex::new(None))`)
- state.rs:167 -- `try_start_op` -- `*current = Some(RunningOp { ... })` (OVERWRITES previous)
- state.rs:186-188 -- `finish_op` -- checks id match, clears to None
- state.rs:200-202 -- `fail_op` -- checks id match, clears to None
- state.rs:211-212 -- `kill_current` -- takes the Option
- routes.rs:165 -- `status` handler reads current op
- routes.rs:1947 -- `refresh_backup_counts` checks if running

**Critical finding:** Single-slot `Option<RunningOp>`. When `allow_parallel=true` and multiple ops start, `try_start_op` at line 167 unconditionally sets `*current = Some(...)`, silently discarding the previous RunningOp. This means:
1. Only the LAST started operation can be killed via `kill_current()`
2. `status` endpoint only shows the LAST started operation
3. `finish_op`/`fail_op` only clear if `op.id == id` -- but if overwritten, the old op's completion goes to ActionLog only (RunningOp already gone)

### AppState::finish_op (state.rs:180)

**Signature:** `pub async fn finish_op(&self, id: u64)`

**References (15):**
- 11 route handlers (one per operation endpoint)
- 3 auto_resume branches
- 1 definition

### AppState::fail_op (state.rs:194)

**Signature:** `pub async fn fail_op(&self, id: u64, error: String)`

**References (19):**
- 11 route handlers (some have multiple fail paths)
- 3 auto_resume branches
- 1 post_actions generic error path
- 1 definition
- Some handlers have 2 fail calls (e.g., restore with db_mapping parse error)

### PidLock::acquire (lock.rs:36)

**Signature:** `pub fn acquire(path: &Path, command: &str) -> Result<Self, ChBackupError>`

**References (6):**
- lock.rs:36 -- definition
- lock.rs:184 -- test `test_acquire_release`
- lock.rs:202-203 -- test `test_double_acquire_fails`
- lock.rs:230 -- test `test_stale_lock_overridden`
- main.rs:127 -- CLI command dispatch (sole production call site)

**Analysis:** Only called from main.rs for CLI commands. Server mode uses `LockScope::None` so PidLock is never acquired. The TOCTOU race at lines 38-72 (`path.exists()` then `fs::write()`) is a real but low-probability issue since chbackup typically has one CLI instance per host.

### sanitize_name (clickhouse/client.rs:1293)

**Signature:** `pub fn sanitize_name(name: &str) -> String`

**References (8):**
- clickhouse/client.rs:1293 -- definition
- clickhouse/client.rs:1311-1313 -- used in `freeze_name()` (FREEZE name construction)
- clickhouse/mod.rs:4 -- re-exported
- list.rs:16 -- imported
- list.rs:1101 -- used in `clean_shadow()` for shadow dir matching
- Tests: 1497-1502

**Analysis:** This function is for FREEZE name sanitization (replaces special chars with underscores), NOT for path traversal validation. A new `validate_backup_name()` function is needed.

### CancellationToken (tokio_util)

**Verified APIs:**
- `CancellationToken::new()` -- creates new token
- `token.clone()` -- creates linked clone (cancelling parent cancels child)
- `token.cancel()` -- cancels token and all clones
- `token.is_cancelled() -> bool` -- sync check
- `token.cancelled() -> impl Future` -- async wait until cancelled

These are the APIs available for wiring cancellation into operation functions.

### retention_local / retention_remote (list.rs)

**Signatures:**
- `pub fn retention_local(data_path: &str, keep: i32) -> Result<usize>` (list.rs:735)
- `pub async fn retention_remote(s3: &S3Client, keep: i32) -> Result<usize>` (list.rs:1003)
- `pub fn effective_retention_local(config: &Config) -> i32` (list.rs:704)
- `pub fn effective_retention_remote(config: &Config) -> i32` (list.rs:717)

**Call sites:**
- watch/mod.rs:490-496 -- `retention_local` (in watch loop after upload)
- watch/mod.rs:510-512 -- `retention_remote` (in watch loop after upload)
- NO calls from upload/mod.rs
- NO calls from server/routes.rs

**Analysis:** The upload command does NOT call retention. Only the watch loop does. The design doc section 3.6 step 7 specifies retention should happen after upload. This is a missing feature.

### upload::upload (upload/mod.rs:175)

**Signature:** `pub async fn upload(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool, diff_from_remote: Option<&str>, resume: bool) -> Result<()>`

**Analysis:** The function does NOT accept a CancellationToken parameter. Adding one would require updating all 6 call sites (3 in routes.rs, 1 in auto_resume, 2 in post_actions).

## Boilerplate Pattern Analysis

The following pattern repeats across ALL 11 operation handlers in routes.rs:

```rust
// Pattern: try_start_op -> spawn -> load config/clients -> call op -> metrics -> finish/fail
let (id, _token) = state.try_start_op("CMD").await.map_err(|e| {
    (StatusCode::LOCKED, Json(ErrorResponse { error: e.to_string() }))
})?;
let state_clone = state.clone();
tokio::spawn(async move {
    let config = state_clone.config.load();
    let ch = state_clone.ch.load();        // some handlers
    let s3 = state_clone.s3.load();        // some handlers
    let start_time = std::time::Instant::now();
    let result = operation_fn(...).await;
    let duration = start_time.elapsed().as_secs_f64();
    match result {
        Ok(_) => {
            if let Some(m) = &state_clone.metrics { /* 3-4 metric calls */ }
            state_clone.finish_op(id).await;
        }
        Err(e) => {
            if let Some(m) = &state_clone.metrics { /* 2-3 metric calls */ }
            state_clone.fail_op(id, e.to_string()).await;
        }
    }
});
Ok(Json(OperationStarted { id, status: "started".to_string() }))
```

The ~40 lines of boilerplate per handler could be extracted into a helper function, reducing routes.rs by approximately 400 lines.

## Backup Name Path Traversal Analysis

Backup names are used in path construction WITHOUT validation at these locations:

| File | Line | Construction |
|------|------|-------------|
| backup/mod.rs | ~281 | `PathBuf::from(&config.clickhouse.data_path).join("backup").join(backup_name)` |
| upload/mod.rs (routes) | 709-711 | `PathBuf::from(&config.clickhouse.data_path).join("backup").join(&name)` |
| routes.rs:288 | post_actions | `PathBuf::from(...).join("backup").join(&backup_name)` |
| routes.rs:354 | post_actions create_remote | same |
| routes.rs:1008 | create_remote handler | same |
| list.rs:500+ | delete_local | `data_path.join(&name)` |
| state.rs:322-324 | auto_resume | same |

**Attack vector:** A name like `../../../tmp/malicious` in the API body or URL path parameter would traverse outside the backup directory. URL path params (axum `Path(name)`) are URL-decoded, so `%2e%2e%2f` would become `../`.

## Config Reload Semantics Analysis

Current behavior of `/api/v1/reload`:
- **With watch active:** Sends `true` on `watch_reload_tx`. Watch loop receives signal during next `interruptible_sleep()`, calls `apply_config_reload()` which reloads config into `WatchContext.config` only. Does NOT update `AppState.config`.
- **Without watch:** Logs "Config reload requested (no watch loop active)" and returns `{"status":"reloaded"}` -- does NOTHING.

Current behavior of `/api/v1/restart`:
- Reloads config from disk, creates NEW ChClient and S3Client, pings CH, atomically swaps ALL THREE into AppState.
- This is the correct full reload, but it creates new connections unnecessarily if only config changed.

The gap: reload should update AppState.config (but not recreate clients) even without watch mode. Currently, non-watch server mode has no way to reload config without full restart.
