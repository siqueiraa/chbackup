# Plan: Server API Kill, Concurrency, and Safety Fixes

## Goal

Fix critical correctness issues in the server API: make the kill endpoint actually cancel operations, support parallel operation tracking, fix PID lock TOCTOU race, add backup name path traversal validation, add upload auto-retention, fix reload semantics, extract DRY orchestration helper, and expand integration tests.

## Architecture Overview

The chbackup HTTP server (`src/server/`) uses `AppState` (shared via axum `State<AppState>`) to manage operation lifecycle. Operations are started with `try_start_op()` which acquires a semaphore permit and creates a `CancellationToken`, then spawned via `tokio::spawn`. The kill endpoint cancels the token, but currently the token is discarded (`_token`) by all 11 route handlers and never passed to spawned tasks. The `current_op` field is a single-slot `Option<RunningOp>` that silently overwrites when `allow_parallel=true`. This plan fixes these issues and adds several safety improvements.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **AppState**: Created by `AppState::new()` in `src/server/state.rs:102`, stored by axum `State` extractor (shared via Clone), accessed by all route handlers
- **RunningOp**: Created by `try_start_op()` at `state.rs:150`, stored in `AppState.current_op`, accessed by `kill_current()`, `finish_op()`, `fail_op()`
- **CancellationToken**: Created in `try_start_op()` at `state.rs:160`, clone stored in `RunningOp.cancel_token`, currently DISCARDED by all 14 call sites (`_token`)
- **PidLock**: Created by `PidLock::acquire()` in `main.rs:127`, stored as local `_lock_guard`, NOT used by server mode (server gets `LockScope::None`)
- **ActionLog**: Created in `AppState::new()` at `state.rs:134`, stores all operations as `VecDeque<ActionEntry>` (ring buffer, capacity 100)
- **Config (server)**: Stored in `AppState.config` via `ArcSwap`, accessed by handlers via `.load()`
- **Config (watch)**: Stored in `WatchContext.config` as `Arc<Config>`, updated by `apply_config_reload()`, NOT synced with `AppState.config`

### What This Plan CANNOT Do
- **Cannot add cooperative cancellation inside long-running ClickHouse queries**: The `clickhouse` crate does not expose query cancellation. Kill can only abort *between* steps, not mid-query.
- **Cannot retroactively cancel already-in-progress sub-operations**: Once an inner function (e.g., `backup::create`) is entered, cancellation requires that function to check the token. This plan uses `tokio::select!` at the spawn boundary, which aborts the task but does not run cleanup inside the function.
- **Cannot make PID locks serialise CLI and server operations**: Server uses semaphore (not PID lock). CLI + server concurrent access on same host is architecturally unsupported by design.

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Kill aborts task without cleanup (UNFREEZE, temp files) | YELLOW | Document that kill is best-effort; UNFREEZE cleanup via `clean_shadow`; future work for cooperative cancel |
| HashMap RunningOp requires updating all status/metrics readers | GREEN | Bounded change: `status`, `refresh_backup_counts`, `kill_op` -- all verified via references.md |
| DRY refactor accidentally changes handler behavior | YELLOW | Each handler has per-operation metrics labels and cache invalidation; helper must parameterize these. Test by comparing cargo test before/after. |
| Reload creating new clients when only config needed | GREEN | Reload creates new ChClient+S3Client same as restart; lightweight since it skips the CH ping |
| Backup name validation may reject previously-accepted names | GREEN | Only rejects names with `..`, `/`, `\`, NUL -- these are never valid backup names |
| Upload auto-retention double-applying in watch loop | GREEN | Retention call gated by explicit `from_watch: bool` parameter or only in non-watch callers |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Starting create operation` | yes | Existing log from route handlers |
| `Operation killed` | yes (when kill invoked) | Existing log from kill_op handler |
| `Config reloaded` | yes (when reload invoked) | New log from reload handler |
| `retention applied after upload` | yes (when retention configured) | New log from upload auto-retention |
| `backup name rejected` | yes (when traversal attempted) | New log from validation |
| `ERROR:.*lock held by PID` | no (forbidden) | Should NOT appear during normal operation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Cooperative cancellation inside backup::create/upload::upload | Requires modifying all 4 command pipelines to accept + check CancellationToken at checkpoints | Phase 10+ |
| CLI + server concurrent access serialization | Architectural design decision -- server uses semaphore, CLI uses PID lock | N/A (by design) |
| T11-T28 integration tests | Large test expansion beyond current scope | Future plan |
| create --resume implementation | Design doc explicitly defers this; current info log is correct | Future plan per design doc |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: Backup name validation function
  - Task 2: PID lock TOCTOU race fix

Group B (Operation Management -- Sequential):
  - Task 3: Multi-op RunningOp tracking (current_op -> running_ops HashMap)
  - Task 4: Kill endpoint wiring (CancellationToken passed to spawned tasks + select!)
  - Task 5: DRY orchestration helper extraction

Group C (Behavior Fixes -- Sequential, depends on Group B):
  - Task 6: Reload semantics fix (reload updates AppState config+clients)
  - Task 7: Upload auto-retention (CLI + API, not watch)

Group D (Documentation -- Sequential, depends on all above):
  - Task 8: Acknowledge create --resume as intentionally deferred
  - Task 9: Integration test expansion (T4-T10)
  - Task 10: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: Backup Name Validation Function

**Purpose:** Prevent path traversal attacks via malicious backup names in API requests and CLI.

**TDD Steps:**
1. Write failing tests in `src/server/state.rs`:
   - `test_validate_backup_name_valid`: normal names like `daily-2024-01-15`, `my_backup`, `backup.v2` should pass
   - `test_validate_backup_name_rejects_dotdot`: `../etc/passwd`, `foo/../bar` should fail
   - `test_validate_backup_name_rejects_slash`: `foo/bar`, `/abs` should fail
   - `test_validate_backup_name_rejects_backslash`: `foo\bar` should fail
   - `test_validate_backup_name_rejects_empty`: empty string should fail
   - `test_validate_backup_name_rejects_nul`: string with NUL byte should fail
2. Implement `pub fn validate_backup_name(name: &str) -> Result<(), &'static str>` in `src/server/state.rs`
   - Reject if empty
   - Reject if contains `..`
   - Reject if contains `/` or `\`
   - Reject if contains NUL byte (`\0`)
   - Return `Ok(())` otherwise
3. Wire into all API entry points that accept backup names:
   - `create_backup` (from `req.backup_name`)
   - `upload_backup` (from URL `Path(name)`)
   - `download_backup` (from URL `Path(name)`)
   - `restore_backup` (from URL `Path(name)`)
   - `create_remote` (from `req.backup_name`)
   - `restore_remote` (from URL `Path(name)`)
   - `delete_backup` (from URL `Path(name)`)
   - `post_actions` (from parsed command string)
4. Return `400 Bad Request` with `ErrorResponse` when validation fails
5. Also wire into CLI `main.rs` `resolve_backup_name()` and `backup_name_required()` paths
6. Verify all tests pass

**Files:** `src/server/state.rs`, `src/server/routes.rs`, `src/main.rs`
**Acceptance:** F001

**Implementation Notes:**
- Place `validate_backup_name` in `state.rs` (alongside `try_start_op`) since it's server-centric validation. Export it pub for CLI use.
- In route handlers, validate BEFORE calling `try_start_op` to avoid consuming a semaphore permit for invalid requests.
- In `create_backup` and `create_remote`, the backup name may be `None` (auto-generated from timestamp) -- skip validation for auto-generated names since they cannot contain traversal chars.

---

### Task 2: PID Lock TOCTOU Race Fix

**Purpose:** Fix the time-of-check-to-time-of-use race in `PidLock::acquire()` where `path.exists()` + `fs::write()` is non-atomic.

**TDD Steps:**
1. Write failing test `test_acquire_atomic_creation` in `src/lock.rs`:
   - Verify that `PidLock::acquire()` uses atomic file creation (test by checking file mode or content after concurrent attempts)
2. Refactor `PidLock::acquire()` to use `OpenOptions::new().write(true).create_new(true)` for initial creation attempt:
   - If `create_new` succeeds: write LockInfo JSON, return Ok
   - If `create_new` fails with `AlreadyExists`: read existing file, check PID liveness, if dead -> remove and retry with `create_new`
   - If `create_new` fails with other error: return Err
3. Keep existing `is_pid_alive()` behavior (no change to platform-specific code)
4. Verify existing tests still pass (`test_acquire_release`, `test_double_acquire_fails`, `test_stale_lock_overridden`)
5. Verify `cargo check` passes with zero warnings

**Files:** `src/lock.rs`
**Acceptance:** F002

**Implementation Notes:**
- The `#[cfg(not(unix))]` block at `lock.rs:157-161` is intentionally inactive on macOS/Linux. This is correct behavior (platform-specific code), not a bug.
- Use `std::fs::OpenOptions` which is already available (`use std::fs`). No new imports needed.
- The `create_new(true)` flag maps to `O_CREAT|O_EXCL` on Unix, providing kernel-level atomicity.

---

### Task 3: Multi-op RunningOp Tracking

**Purpose:** Replace single-slot `current_op: Option<RunningOp>` with `running_ops: HashMap<u64, RunningOp>` to properly track parallel operations when `allow_parallel=true`.

**TDD Steps:**
1. Write failing tests in `src/server/state.rs`:
   - `test_running_ops_tracks_multiple`: start 3 ops, verify all 3 are in the map
   - `test_running_ops_finish_removes`: start 2 ops, finish one, verify one remains
   - `test_running_ops_fail_removes`: start op, fail it, verify map is empty
   - `test_running_ops_kill_by_id`: start 2 ops, kill one by ID, verify other survives
2. Replace `current_op: Arc<Mutex<Option<RunningOp>>>` with `running_ops: Arc<Mutex<HashMap<u64, RunningOp>>>` in `AppState`
3. Update `AppState::new()`: initialize as `Arc::new(Mutex::new(HashMap::new()))`
4. Update `try_start_op()`: insert into HashMap instead of overwriting Option
5. Update `finish_op(id)`: remove by ID from HashMap
6. Update `fail_op(id, error)`: remove by ID from HashMap
7. Update `kill_current() -> bool`: rename to `kill_op(id: Option<u64>) -> bool`:
   - If `id` is `Some(id)`: cancel and remove specific op
   - If `id` is `None`: cancel ALL running ops (backward-compatible kill-all)
8. Update `status()` handler: return list of running ops (or first if single)
9. Update `refresh_backup_counts()`: check `running_ops.is_empty()` for `in_progress` gauge
10. Update `kill_op()` route handler: add query-parameter struct and change signature:
    ```rust
    #[derive(Deserialize, Default)]
    struct KillParams { id: Option<u64> }

    pub async fn kill_op(
        State(state): State<AppState>,
        Query(params): Query<KillParams>,
    ) -> Result<&'static str, StatusCode>
    ```
    Pass `params.id` to `state.kill_op(params.id).await`.
11. Verify all tests pass, `cargo check` passes

**Files:** `src/server/state.rs`, `src/server/routes.rs`
**Acceptance:** F003

**Implementation Notes:**
- Add `use std::collections::HashMap;` to `state.rs` imports (stdlib, no new crate dependency).
- The `status()` handler currently returns a single `StatusResponse`. For backward compatibility, keep returning the "first" running op (or idle). A future plan can add a `/api/v1/status/all` endpoint.
- The `refresh_backup_counts()` function at `routes.rs:1947` reads `current_op.lock().await.is_some()` -- change to `running_ops.lock().await.is_empty()` (negated).
- The `kill_op` handler at `routes.rs:1416` currently calls `state.kill_current()`. Change to parse optional `?id=N` query param and call `state.kill_op(id)`.
- Redundancy: this REPLACES `current_op`. After migration, grep must find zero references to `current_op`.

---

### Task 4: Kill Endpoint Wiring

**Purpose:** Make the kill endpoint actually stop running operations by wiring the CancellationToken into spawned tasks.

**TDD Steps:**
1. Write failing test `test_cancellation_token_aborts_task` in `src/server/state.rs`:
   - Create a CancellationToken, spawn a task with `tokio::select!`, cancel token, verify task exits
2. In ALL 11 route handlers in `routes.rs`, change `let (id, _token)` to `let (id, token)`:
   - In the `tokio::spawn` block, wrap the operation call with:
     ```rust
     tokio::select! {
         _ = token.cancelled() => {
             warn!("Operation {} killed by user", id);
             state_clone.fail_op(id, "killed by user".to_string()).await;
         }
         result = operation_fn(...) => {
             // existing match result { Ok/Err } handling
         }
     }
     ```
   - The 11 handlers are: `create_backup`, `upload_backup`, `download_backup`, `restore_backup`,
     `create_remote`, `restore_remote`, `delete_backup`, `clean_remote_broken`, `clean_local_broken`,
     `clean`, and the single dispatch block in `post_actions`.
   - **EXCLUDE** the 3 `_token` discards in `auto_resume()` (state.rs) — auto-resume operations are
     fire-and-forget restart recovery and must NOT be cancellable via kill.
3. Verify that when `kill_op(id)` is called:
   - The token is cancelled
   - The `tokio::select!` branch fires
   - The operation is marked as failed/killed in ActionLog
4. Verify all 11 handlers in routes.rs have no remaining `_token` binding (grep returns 0)
5. Verify all tests pass

**Files:** `src/server/routes.rs`, `src/server/state.rs`
**Acceptance:** F004

**Implementation Notes:**
- The `token.cancelled()` future resolves immediately if the token was already cancelled. This is safe even if kill happens before the operation starts.
- When the cancelled branch fires, the operation function's future is DROPPED. This means any in-progress I/O is aborted but no cleanup runs. Document this as a known limitation (UNFREEZE cleanup via `clean_shadow`).
- For `create_remote` and `restore_remote` (compound ops), wrap the ENTIRE compound block, not individual steps.
- Task 5 (DRY) will extract this `tokio::select!` pattern into a helper. Task 4 fully implements it in all 11 handlers first so that F004 acceptance passes independently before Task 5 refactors.

---

### Task 5: DRY Orchestration Helper Extraction

**Purpose:** Extract the repeated try_start_op/spawn/select!/metrics/finish_op/fail_op boilerplate from all 11 route handlers into a single helper function.

**TDD Steps:**
1. Write test `test_run_operation_success` and `test_run_operation_failure` in `src/server/routes.rs` (or `state.rs`):
   - Verify the helper calls finish_op on success, fail_op on failure
   - Verify metrics are recorded
2. Define a helper function in `state.rs`:
   ```rust
   pub async fn run_operation<F, Fut>(
       state: &AppState,
       command: &str,
       op_label: &str,
       invalidate_cache: bool,
       f: F,
   ) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)>
   where
       F: FnOnce(Arc<Config>, Arc<ChClient>, Arc<S3Client>) -> Fut + Send + 'static,
       Fut: Future<Output = Result<()>> + Send,
   ```
   - Calls `try_start_op(command)` with 423 error mapping
   - Clones state
   - `tokio::spawn` with `tokio::select!` (cancellation branch from Task 4)
   - Records duration, success/failure metrics with `op_label`
   - Calls `finish_op`/`fail_op`
   - If `invalidate_cache`: calls `manifest_cache.lock().await.invalidate()`
   - Returns `Ok(Json(OperationStarted { id, status: "started" }))`
3. Replace the 10 standalone operation handlers (all except `post_actions`) to use the helper. Each becomes:
   - Parse request body
   - Validate backup name (from Task 1)
   - Call `run_operation(state, "cmd", "cmd_label", cache_flag, |config, ch, s3| async move { ... })`
4. Handle special cases:
   - `create_backup`: the closure captures `state_clone.metrics` and records `backup_size_bytes` and
     `backup_last_success_timestamp` directly before returning `Ok(())`. No manifest return needed from
     the closure — metrics are recorded inside it:
     ```rust
     |config, ch, s3| async move {
         let manifest = crate::backup::create(...).await?;
         if let Some(m) = &state_clone.metrics {
             m.backup_size_bytes.set(manifest.compressed_size as f64);
             m.backup_last_success_timestamp.set(Utc::now().timestamp() as f64);
         }
         Ok(())
     }
     ```
   - `create_remote`: compound operation (create then upload) -- single closure wrapping both steps
   - `post_actions`: **explicitly out of scope for this task**. `post_actions` returns
     `Result<(StatusCode, Json<OperationStarted>), ...>` (201 CREATED), which is incompatible with the
     helper's `Result<Json<OperationStarted>, ...>` return type. `post_actions` retains its inline
     `try_start_op` + `tokio::spawn` pattern. The cancellation wiring from Task 4 (the `tokio::select!`)
     is already in `post_actions`' spawn block; the DRY refactor is not applied to it.
5. Verify all existing tests pass
6. Verify `cargo check` and `cargo test` pass

**Files:** `src/server/state.rs`, `src/server/routes.rs`
**Acceptance:** F005

**Implementation Notes:**
- The helper closure signature `FnOnce(Arc<Config>, Arc<ChClient>, Arc<S3Client>) -> Fut` where
  `Fut: Future<Output = Result<()>>` covers all handlers. Operations that only need `ch` or `s3`
  simply ignore the unused argument in the closure.
- **Closures record their own operation-specific metrics**: Each closure captures `Arc` clones of
  `state_clone.metrics` and records labels like `backup_size_bytes` inside the closure body before
  returning `Ok(())`. The helper itself records only generic duration/success/error metrics.
- `post_actions` is explicitly excluded (incompatible return type). Document this exclusion in a code
  comment in `routes.rs` adjacent to `post_actions`.
- After this task, grep for `try_start_op` in `routes.rs` should return exactly 1 result (in
  `post_actions` only). `try_start_op` in `state.rs` is also used by the helper and `auto_resume`.

---

### Task 6: Reload Semantics Fix

**Purpose:** Make `/api/v1/reload` update `AppState.config`, `AppState.ch`, and `AppState.s3` even
when watch mode is not active. Also fix `apply_config_reload()` in the watch loop to rotate
`WatchContext.ch` and `WatchContext.s3` — currently only `WatchContext.config` is updated, leaving
the watch loop with stale clients after a credential or endpoint change.

**TDD Steps:**
1. Write test `test_reload_updates_config` in `src/server/routes.rs`:
   - Verify that after reload, `state.config.load()` returns the new config
2. Extract a helper that loads config and creates new clients WITHOUT swapping:
   ```rust
   async fn reload_config_and_clients(
       state: &AppState,
   ) -> Result<(Config, ChClient, S3Client), (StatusCode, Json<ErrorResponse>)>
   ```
   - Loads config via `Config::load(&state.config_path, &[])` + `validate()`
   - Creates new `ChClient::new(&config.clickhouse)` and `S3Client::new(&config.s3).await`
   - Returns the three new values; does **NOT** call `ArcSwap::store()` itself
3. Refactor `reload()` handler to:
   - Call `reload_config_and_clients(&state).await?` to get `(config, ch, s3)`
   - Atomically swap all three via `state.config.store(Arc::new(config))` etc.
   - If watch active: send reload signal to watch loop
   - Log "Config reloaded"
4. Refactor `restart()` handler to:
   - Call `reload_config_and_clients(&state).await?` to get `(config, ch, s3)`
   - Ping ClickHouse: `ch.ping().await` — if ping fails, return error WITHOUT swapping
     (preserves existing "old clients remain active on ping failure" guarantee)
   - On ping success: atomically swap all three
   - Log "Restart completed"
5. Make `apply_config_reload()` in `src/watch/mod.rs` async and add client recreation:
   - Change signature to `async fn apply_config_reload(ctx: &mut WatchContext)`
   - After loading and validating the new config, recreate clients:
     ```rust
     let new_ch = match ChClient::new(&new_config.clickhouse) {
         Ok(c) => c,
         Err(e) => { warn!(error=%e, "watch: reload failed to recreate ChClient (keeping old)"); return; }
     };
     let new_s3 = match S3Client::new(&new_config.s3).await {
         Ok(c) => c,
         Err(e) => { warn!(error=%e, "watch: reload failed to recreate S3Client (keeping old)"); return; }
     };
     ctx.ch = new_ch;
     ctx.s3 = new_s3;
     ctx.config = Arc::new(new_config);   // update config last, only after both clients succeed
     ```
   - Update the call site in `interruptible_sleep()` to `.await` the now-async function
   - Client recreation failures are non-fatal: log warning and keep old config+clients intact
6. Verify tests pass

**Files:** `src/server/routes.rs`, `src/server/state.rs`, `src/watch/mod.rs`
**Acceptance:** F006

**Implementation Notes:**
- The helper returns a tuple rather than performing the swap itself. This preserves `restart()`'s
  atomicity guarantee: if the CH ping fails, no swap has occurred and the old clients remain active.
- Both callers must call `Config::load(&state.config_path, &[])` (via the helper).
- `apply_config_reload()` becoming async is safe: its call site in `interruptible_sleep()` is already
  inside an `async fn` using `tokio::select!`. Add `.await` at the call site.
- Config is updated last (after both clients succeed) so a partial failure leaves the watch loop in a
  fully consistent state — either all three are new or all three remain old.
- After this fix, both `AppState.ch/s3` AND `WatchContext.ch/s3` are rotated on reload.

---

### Task 7: Upload Auto-Retention

**Purpose:** After successful upload (CLI and API), apply local and remote retention per `effective_retention_local/remote()` config. Watch loop already handles its own retention.

**TDD Steps:**
1. Write test `test_retention_after_upload_called` in `src/upload/mod.rs` or `src/server/routes.rs`:
   - Verify retention functions are called with correct keep values after upload
2. Create `pub async fn apply_retention_after_upload(config: &Config, s3: &S3Client, manifest_cache: Option<&Mutex<ManifestCache>>)` in `src/list.rs`:
   - Calls `effective_retention_local(config)` and `effective_retention_remote(config)`
   - If local keep > 0: `spawn_blocking(retention_local(data_path, keep))`
   - If remote keep > 0: `retention_remote(s3, keep)` then invalidate manifest cache if provided
   - All errors are warnings (best-effort, matching watch loop pattern from `watch/mod.rs:487-527`)
3. Wire into:
   - CLI `main.rs` upload command (after `upload::upload()` returns Ok): call `apply_retention_after_upload(config, s3, None)`
   - API `upload_backup` handler (after upload Ok in spawned task): call `apply_retention_after_upload(config, s3, Some(&state.manifest_cache))`
   - API `create_remote` handler (after upload Ok in spawned task): same
   - API `post_actions` upload branch: same
4. Do NOT add to watch loop (it already handles retention at `watch/mod.rs:487-527`)
5. Do NOT add to auto_resume upload path (resume should not trigger retention)
6. Verify tests pass

**Files:** `src/list.rs`, `src/main.rs`, `src/server/routes.rs`
**Acceptance:** F007

**Implementation Notes:**
- The watch loop pattern (lines 490-527) is the reference implementation. The new helper should produce identical behavior (same log messages, same best-effort error handling).
- `retention_local` is a sync function -- must use `spawn_blocking` (already done in watch loop).
- `retention_remote` is async -- can call directly.
- The `ManifestCache` parameter is `Option` because CLI mode has no cache.
- Design doc section 3.6 step 7 says "Apply retention: delete oldest remote backups exceeding `backups_to_keep_remote`" after upload. This is the authority for this change.

---

### Task 8: Acknowledge create --resume as Intentionally Deferred

**Purpose:** Document that `create --resume` is correctly handled (info log, no-op) and not a bug.

**TDD Steps:**
1. Verify the existing code at `main.rs:157-159` already logs `"--resume flag has no effect on the create command"` -- no code change needed.
2. Add a code comment explaining the design decision:
   ```rust
   // Design doc: create --resume is planned but explicitly deferred. The create
   // command operates on local filesystem only (FREEZE + hardlink) with no remote
   // state to resume from. Resume is meaningful for upload/download/restore which
   // interact with S3 and can be interrupted mid-transfer.
   ```
3. No test changes needed -- the existing behavior is correct.

**Files:** `src/main.rs` (comment only)
**Acceptance:** F008

**Implementation Notes:**
- This is a documentation-only task. No behavioral change.
- The comment clarifies intent for future developers who might think this is a bug.

---

### Task 9: Integration Test Expansion (T4-T10)

**Purpose:** Add integration tests for incremental backup chain, schema-only backup, partitioned restore, server API endpoints, and backup name validation.

**TDD Steps:**
1. Add test functions to `test/run_tests.sh` following existing pattern:
   - `test_incremental_chain` (T4): Create full backup, insert more data, create incremental with `--diff-from`, upload both, download incremental, restore, verify all data present
   - `test_schema_only` (T5): Create with `--schema`, verify no data in backup, restore schema only
   - `test_partitioned_restore` (T6): Create backup of partitioned table, restore with `--partitions`, verify only selected partitions restored
   - `test_server_api_create_upload` (T7): Start server in background, POST /api/v1/create, wait for completion via /api/v1/actions, POST /api/v1/upload, verify via /api/v1/list, cleanup
   - `test_backup_name_validation` (T8): Via CLI, attempt `chbackup create "../malicious"` and verify it fails with validation error
   - `test_delete_and_list` (T9): Create + upload, verify in list, delete remote, verify gone, delete local, verify gone
   - `test_clean_broken` (T10): Create a backup, corrupt its metadata.json, run `chbackup clean_broken local`, verify it's cleaned
2. Register all tests in the `should_run` dispatch at the end of the script
3. Verify each test function follows the existing pass/fail/skip pattern

**Files:** `test/run_tests.sh`
**Acceptance:** F009

**Implementation Notes:**
- Tests must be idempotent: create unique backup names with `$$` PID suffix
- Tests must clean up after themselves (delete remote/local backups)
- T7 (server test) requires starting the server in background (`chbackup server &`), waiting for ready, then using `curl`. Use `kill $SERVER_PID` for cleanup.
- T6 (partitioned restore) requires a partitioned table. Add a `CREATE TABLE ... PARTITION BY` to fixtures if not already present.
- All tests gate on `should_run "test_name"` for selective execution.

---

### Task 10: Update CLAUDE.md for All Modified Modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** `src/server`, `src/upload`, `src/backup`

**TDD Steps:**

1. **Read affected-modules.json for module list:**
   - `src/server` (routes.rs, state.rs, mod.rs changes)
   - `src/upload` (auto-retention change -- only called from routes.rs/main.rs, upload/mod.rs unchanged)
   - `src/backup` (no changes to backup module itself -- validation is in state.rs)

2. **For `src/server/CLAUDE.md`:**
   - Replace all references to `current_op: Arc<Mutex<Option<RunningOp>>>` with `running_ops: HashMap<u64, RunningOp>` — this field appears in the AppState Sharing section, Operation Lifecycle section, and scrape-time refresh description
   - Update "Operation Lifecycle" section to document the HashMap-based tracking and `kill_op(id: Option<u64>)`
   - Update "Route Handler Delegation Pattern" to show `run_operation()` helper usage; note `post_actions` is excluded (incompatible return type)
   - Add "Kill Endpoint" section documenting CancellationToken wiring via `tokio::select!` and the known limitation (no cleanup on abort)
   - Add "Backup Name Validation" section documenting `validate_backup_name()`
   - Update "Reload Endpoint" section to document that reload now updates AppState config+clients via `reload_config_and_clients()` helper
   - Update "Restart Endpoint" section to show shared `reload_config_and_clients()` helper + ping ordering
   - Add "Upload Auto-Retention" pattern reference
   - Regenerate directory tree

3. **Validate all CLAUDE.md files:**
   - Verify required sections: Parent Context, Directory Structure, Key Patterns, Parent Rules
   - Verify parent link is correct (`../../CLAUDE.md`)
   - Verify `current_op` does NOT appear anywhere in the updated `src/server/CLAUDE.md`

**Files:** `src/server/CLAUDE.md`
**Acceptance:** FDOC

**Notes:**
- This task runs AFTER all code tasks complete
- `src/upload/CLAUDE.md` and `src/backup/CLAUDE.md` do NOT need updates since their source files are not changed by this plan (retention helper is in `list.rs`, validation is in `state.rs`)
- Preserve existing patterns, only ADD new ones

---

## Notes

### Phase 4.5 (Interface Skeleton Simulation): SKIPPED
**Reason:** All changes are within existing functions and modules. No new imports or types are introduced that aren't already verified in `context/symbols.md` and `context/knowledge_graph.json`. The key types (`CancellationToken`, `HashMap`, `AppState`, `RunningOp`) are all verified as existing and compilable.

### create --resume Acknowledgment
The design doc says `--resume` on create is planned but explicitly deferred. The current code's info log at `main.rs:157-159` is correct behavior. No code change needed -- Task 8 adds a clarifying comment only.

### PID Lock in Server Mode
Server API operations use the AppState semaphore (`op_semaphore`) as the serialization mechanism. The design's "PID lock" intent for API ops is already satisfied by the semaphore. No new PID lock needed in server mode -- this is documented in the architecture assumptions.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (Symbols match) | PASS | All symbols verified in knowledge_graph.json |
| RC-016 (Tests match implementation) | PASS | Test names match function names in TDD steps |
| RC-017 (Acceptance IDs match tasks) | PASS | F001-F009 + FDOC mapped to Tasks 1-10 |
| RC-018 (Dependencies satisfied) | PASS | Group B (Tasks 3-5) sequential; Group C (Tasks 6-7) depends on Group B; Group D (Tasks 8-10) depends on all |
| RC-008 (TDD sequencing) | PASS | Task 4 (kill wiring) uses running_ops from Task 3; Task 5 (DRY) uses both; Task 6-7 use DRY helper from Task 5 |
| RC-019 (Existing pattern followed) | PASS | DRY helper matches exact pattern from routes.rs handlers; retention helper matches watch/mod.rs:490-527 |

### Redundancy Checks

| REPLACE Decision | Removal Task | Old Code Absent Check |
|------------------|--------------|-----------------------|
| `current_op` -> `running_ops` | Task 3 | F003 structural check greps for `current_op` absence |
| 11 handler boilerplate -> `run_operation` helper | Task 5 | F005 structural check verifies `try_start_op` not in routes.rs |

### Anti-Overengineering Checklist
- [x] `validate_backup_name` is minimal (5 checks, no regex, no dependencies)
- [x] `run_operation` helper is a function, not a trait or macro
- [x] `running_ops` is a `HashMap`, not a concurrent data structure
- [x] Reload reuses restart logic, does not introduce new abstractions
- [x] Integration tests follow existing `test/run_tests.sh` patterns exactly
