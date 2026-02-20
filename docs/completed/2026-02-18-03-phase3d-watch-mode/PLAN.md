# Plan: Phase 3d -- Watch Mode

## Goal

Implement the watch mode scheduler (design doc section 10) that runs as a long-lived process maintaining a full + incremental backup chain automatically. This includes the state machine loop, name template resolution, resume-on-restart, SIGHUP config hot-reload, API endpoints for watch lifecycle (start/stop/status/reload), and integration with the server `--watch` flag.

## Architecture Overview

Watch mode adds a new `src/watch/` module containing the state machine loop, name template resolution, and resume logic. The loop orchestrates existing `backup::create()`, `upload::upload()`, `list::delete_local()`, `list::retention_local()`, and `list::retention_remote()` functions in a cycle. Integration points:

1. **Standalone CLI**: `chbackup watch` runs the loop directly
2. **Server mode**: `chbackup server --watch` spawns the loop as a tokio background task alongside the HTTP server
3. **API control**: `/api/v1/watch/{start,stop,status}` and `/api/v1/reload` replace stubs in routes.rs
4. **Metrics**: Updates existing `watch_state`, `watch_last_full_timestamp`, `watch_last_incremental_timestamp`, `watch_consecutive_errors` gauges in Metrics
5. **SIGHUP**: Sets `reload_pending` flag; applied at next sleep cycle entry

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **WatchConfig**: Defined in config.rs (already exists, 7 fields). Read by watch module.
- **Metrics watch gauges**: Created in metrics.rs (already registered). Updated by watch loop via `Arc<Metrics>`.
- **AppState**: Owns watch lifecycle state (start/stop handle). Extended with watch-specific fields.
- **ChClient**: Owns `get_macros()` method for `{shard}` template resolution.
- **Watch loop**: Spawned by `start_server()` or `main.rs`; communicates via channels for start/stop/reload.

### What This Plan CANNOT Do
- Cannot add `tables` field to WatchConfig without migration concern (existing configs would break if field not optional). Will use `Option<String>` with `#[serde(default)]`.
- Cannot interrupt a running backup create/upload mid-operation for config reload (design 10.8 explicitly forbids this).
- Cannot test resume-on-restart or SIGHUP in unit tests (requires real S3/ClickHouse). Integration tests deferred.
- Cannot make watch loop work with `allow_parallel=false` server ops -- watch cycle holds no op semaphore (it runs alongside the server, not as a server operation).

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| State machine flag leaks (RC-011) | YELLOW | Every flag (`force_next_full`, `reload_pending`, `consecutive_errors`) has explicit reset on every exit path. Tests verify all transitions. |
| SIGHUP not available on non-Unix | GREEN | `tokio::signal::unix` is Unix-only. Build gated with `#[cfg(unix)]`. Server mode primary target is Linux. |
| Config hot-reload race with running backup | GREEN | Design 10.8 specifies: current cycle completes, reload at next sleep entry. No locking needed. |
| `system.macros` table empty or missing | GREEN | `get_macros()` returns empty HashMap on error (no macros to resolve). Template uses literal text for unresolved macros. |
| Watch metrics stale after stop | GREEN | `watch_stop` sets `watch_state=0`. Timestamp gauges retain last values (correct for Prometheus). |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `watch: starting watch loop` | yes | Watch loop entry |
| `watch: resume state` | yes | Resume determination log |
| `watch: creating.*backup` | yes | Each backup cycle start |
| `watch: upload complete` | yes | Upload success |
| `watch: cycle complete.*sleeping` | yes | Sleep between cycles |
| `watch: error.*consecutive_errors` | no (conditional) | Error path with count |
| `watch: config reloaded:.*→` | no (conditional) | SIGHUP reload applied with old→new values (design 10.8) |
| `watch: aborting.*max_consecutive_errors` | no (conditional) | Abort threshold reached |
| `ERROR:.*watch` | no (forbidden on success path) | Should NOT appear on happy path |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| `POST /api/v1/restart` endpoint | Separate from watch mode; requires full server restart logic (originally Phase 3a scope) | Deferred |
| `GET /api/v1/tables` endpoint | Table listing from CH, not watch-related | Phase 4f |
| Watch metrics state enum values (idle/full/incr/sleeping/error) | Metrics uses IntGauge with numeric encoding; design says enum but current registration uses 0/1/2 | Acceptable as-is; document mapping |
| Manifest caching for retention | Design 8.2 mentions server-mode caching | Future optimization |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: Make parse_duration_secs public + add WatchConfig.tables field
  - Task 2: Add ChClient::get_macros() method
  - Task 3: Name template resolution (resolve_name_template)
  - Task 4: Watch resume state (resume_state)

Group B (Core Loop -- Sequential, depends on Group A):
  - Task 5: Watch state machine loop (run_watch_loop)
  - Task 6: Module wiring (lib.rs, main.rs standalone watch command)

Group C (Server Integration -- Sequential, depends on Group B):
  - Task 7: AppState watch fields + watch handle type
  - Task 8: Server watch loop spawn + SIGHUP handler
  - Task 9: Replace API stub endpoints (watch/start, watch/stop, watch/status, reload)

Group D (Documentation -- depends on Group C):
  - Task 10: Update CLAUDE.md for all modified modules
```

## Tasks

### Task 1: Make parse_duration_secs public + add WatchConfig.tables field

**TDD Steps:**
1. Write unit test `test_parse_duration_secs_public_access` in `src/config.rs` tests that calls `parse_duration_secs("1h")` -- this will fail because it is currently private.
2. Change `fn parse_duration_secs` to `pub fn parse_duration_secs` in `src/config.rs:1248`.
3. Write unit test `test_watch_config_tables_field` that creates a `WatchConfig` with `tables: Some("default.*".to_string())` and verifies the field.
4. Add `tables: Option<String>` field with `#[serde(default)]` to `WatchConfig` struct (after `name_template`).
5. Verify both tests pass.

**Files:**
- `src/config.rs` (modify: make fn public, add field)

**Acceptance:** F001

**Notes:**
- `parse_duration_secs` at config.rs:1248 is `fn` not `pub fn`. Making it `pub` is sufficient.
- `tables` field uses `Option<String>` with `#[serde(default)]` so existing YAML configs without this field still parse correctly (default = None = match all).
- The Default impl for WatchConfig needs updating to include `tables: None`.

---

### Task 2: Add ChClient::get_macros() method

**TDD Steps:**
1. Write unit test `test_macro_row_deserializable` that verifies the `MacroRow` struct can be deserialized from a mock JSON representing `system.macros` columns.
2. Add `MacroRow` struct with `#[derive(clickhouse::Row, serde::Deserialize)]` fields: `macro_name: String` (renamed from `macro` via serde), `substitution: String`.
3. Implement `pub async fn get_macros(&self) -> Result<HashMap<String, String>>` on `ChClient`:
   - SQL: `SELECT macro AS macro_name, substitution FROM system.macros`
   - Use `self.inner.query(sql).fetch_all::<MacroRow>()` pattern (following `get_disks()` pattern)
   - Convert `Vec<MacroRow>` to `HashMap<String, String>`
   - On query error, log warning and return empty HashMap (graceful -- system.macros may not exist on all CH versions)
4. Verify test passes.

**Files:**
- `src/clickhouse/client.rs` (modify: add MacroRow, add get_macros method)

**Acceptance:** F002

**Notes:**
- `macro` is a Rust reserved keyword, so the column must be aliased in SQL (`macro AS macro_name`) or use `#[serde(rename = "macro")]`. Using SQL alias is simpler and matches the `get_disks` pattern where `type` is handled similarly.
- Pattern follows `get_disks()` at client.rs:375 exactly: SQL string, conditional logging, `fetch_all`, context error.
- Graceful fallback: returns empty HashMap on error because `system.macros` may not exist in some CH setups.

---

### Task 3: Name template resolution (resolve_name_template)

**TDD Steps:**
1. Write unit test `test_resolve_type_macro` that verifies `{type}` is replaced with `"full"` and `"incr"`.
2. Write unit test `test_resolve_time_macro` that verifies `{time:%Y%m%d_%H%M%S}` is replaced with a formatted timestamp.
3. Write unit test `test_resolve_shard_macro` that verifies `{shard}` is replaced from a macros HashMap.
4. Write unit test `test_resolve_full_template` that verifies the default template `shard{shard}-{type}-{time:%Y%m%d_%H%M%S}` resolves correctly.
5. Write unit test `test_resolve_unknown_macro` that verifies unknown macros like `{unknown}` are left as-is.
6. Implement `pub fn resolve_name_template(template: &str, backup_type: &str, now: DateTime<Utc>, macros: &HashMap<String, String>) -> String`:
   - Replace `{type}` with `backup_type` ("full" or "incr")
   - Replace `{time:FORMAT}` by extracting FORMAT and using `now.format(FORMAT)`
   - Replace `{macro_name}` for each key in macros HashMap
   - Leave unrecognized `{...}` patterns as-is
7. Create `src/watch/mod.rs` with module-level doc comment and this function.
8. Verify all tests pass.

**Files:**
- `src/watch/mod.rs` (create: new module with resolve_name_template)

**Acceptance:** F003

**Notes:**
- The `{time:FORMAT}` pattern requires parsing the `:FORMAT` suffix. Use a regex or manual parse to extract the format string between `{time:` and `}`.
- chrono's `format::strftime` is used for time formatting: `now.format(format_str).to_string()`.
- ClickHouse macros from `system.macros` are simple string substitution: `{shard}` -> value from HashMap.
- Import: `use chrono::{DateTime, Utc};` and `use std::collections::HashMap;`

---

### Task 4: Watch resume state (resume_state)

**TDD Steps:**
1. Write unit test `test_resume_no_backups` -- empty remote list returns `ResumeDecision::FullNow`.
2. Write unit test `test_resume_recent_full_no_incr` -- full backup within watch_interval returns `ResumeDecision::SleepThen { remaining, next_type: "incr" }`.
3. Write unit test `test_resume_stale_full` -- full backup older than full_interval returns `ResumeDecision::FullNow`.
4. Write unit test `test_resume_stale_incr` -- incremental older than watch_interval returns `ResumeDecision::IncrNow { diff_from }`.
5. Write unit test `test_resume_recent_incr` -- incremental within watch_interval returns `ResumeDecision::SleepThen { remaining, next_type: "incr" }`.
6. Define `ResumeDecision` enum:
   ```rust
   pub enum ResumeDecision {
       FullNow,
       IncrNow { diff_from: String },
       SleepThen { remaining: Duration, backup_type: String },
   }
   ```
7. Write unit test `test_resume_filters_by_template_prefix` -- backups not matching the name template prefix are excluded from resume consideration.
8. Implement `pub fn resolve_template_prefix(name_template: &str) -> String` that extracts the static prefix from a name template (everything before the first `{` macro). E.g., `"shard1-{type}-{time:%Y%m%d}"` -> `"shard1-"`.
9. Implement `pub fn resume_state(backups: &[BackupSummary], name_template: &str, watch_interval: Duration, full_interval: Duration, now: DateTime<Utc>) -> ResumeDecision`:
   - Compute template prefix via `resolve_template_prefix(name_template)`
   - Filter non-broken backups whose name starts with the prefix (per design 10.5: "matching name_template pattern")
   - Filter out backups with `None` timestamp, sort by timestamp descending
   - Find most recent backup whose name contains "full" -> `last_full`
   - Find most recent backup whose name contains "incr" -> `last_incr`
   - Calculate elapsed since each
   - Apply decision logic from design 10.5
10. Verify all tests pass.

**Files:**
- `src/watch/mod.rs` (modify: add ResumeDecision, resolve_template_prefix, resume_state)

**Acceptance:** F004

**Notes:**
- **Template prefix filtering** (design 10.5): Remote backups are filtered by the static prefix of the name template before type inference. This prevents picking up unrelated backups from other tools or manual operations in the same S3 prefix. E.g., template `"shard1-{type}-{time:%Y%m%d}"` only considers backups starting with `"shard1-"`.
- Backup type (full vs incr) inferred from backup name containing "full" or "incr" substring. This matches the Go tool behavior and is noted in data-authority.md.
- `BackupSummary.timestamp` is `Option<DateTime<Utc>>` -- skip backups with `None` timestamp.
- Duration comparison uses `chrono::Duration` from `now - timestamp`.
- The `diff_from` for `IncrNow` is the name of the most recent non-broken backup (full or incr).
- Import: `use crate::list::BackupSummary;`

---

### Task 5: Watch state machine loop (run_watch_loop)

**TDD Steps:**
1. Write unit test `test_watch_state_enum_values` verifying `WatchState` variants map to expected metric values (Idle=1, CreatingFull=2, CreatingIncr=3, Uploading=4, Cleaning=5, Sleeping=6, Error=7).
2. Write unit test `test_force_full_after_error` verifying that after an error, the `force_next_full` flag is set to true.
3. Write unit test `test_consecutive_errors_reset_on_success` verifying `consecutive_errors` resets to 0 after a successful cycle.
4. Write unit test `test_consecutive_errors_abort` verifying that `consecutive_errors >= max` returns `WatchLoopExit::MaxErrors`.
5. Define `WatchState` enum: `Idle`, `CreatingFull`, `CreatingIncr`, `Uploading`, `Cleaning`, `Sleeping`, `Error`.
6. Define `WatchLoopExit` enum: `Shutdown`, `MaxErrors`, `Stopped`.
7. Define `WatchContext` struct holding all loop state:
   ```rust
   pub struct WatchContext {
       pub config: Arc<Config>,
       pub ch: ChClient,
       pub s3: S3Client,
       pub metrics: Option<Arc<Metrics>>,
       pub state: WatchState,
       pub consecutive_errors: u32,
       pub force_next_full: bool,
       pub last_backup_name: Option<String>,
       pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
       pub reload_rx: tokio::sync::watch::Receiver<bool>,
   }
   ```
8. Implement `pub async fn run_watch_loop(ctx: WatchContext) -> WatchLoopExit` with the state machine from design 10.4:
   - **Resume**: Call `list_remote()`, then `resume_state()`. Sleep if needed.
   - **Decide**: Check `force_next_full` or `full_interval` elapsed -> full. Otherwise -> incremental.
   - **Create**: Call `backup::create()` with resolved name template. For incremental, pass `diff_from`.
   - **Upload**: Call `upload::upload()` with `delete_local` from config.
   - **Delete local**: If `delete_local_after_upload`, call `list::delete_local()` via `spawn_blocking` (sync fn).
   - **Retention**: Call `list::retention_local()` via `spawn_blocking` (sync fn) and `list::retention_remote()` (async). Log warnings on failure (best-effort per design 10.7).
   - **Sleep**: Wait `watch_interval`. Check `shutdown_rx` and `reload_rx` during sleep.
   - **Error**: Increment `consecutive_errors`, set `force_next_full=true`, sleep `retry_interval`.
   - **Reload**: If `reload_rx` signaled, re-read config from file via `Config::load()`, call `config.validate()` on new config, apply new watch params. Log old→new values: `"Config reloaded: watch_interval=1h→30m, full_interval=24h→12h"` (design 10.8 step 3d). If validation fails, log warning and keep current config.
   - **Metrics**: Update `watch_state`, `watch_last_full_timestamp`, `watch_last_incremental_timestamp`, `watch_consecutive_errors` at each state transition.
9. Verify all tests pass.

**Files:**
- `src/watch/mod.rs` (modify: add WatchState, WatchLoopExit, WatchContext, run_watch_loop)

**Acceptance:** F005, F006

**Notes:**
- `shutdown_rx` and `reload_rx` are `tokio::sync::watch::Receiver<bool>` channels. The sender side is held by the server or main.rs.
- Use `tokio::select!` for interruptible sleep: `tokio::time::sleep(interval)` vs `shutdown_rx.changed()`.
- Config reload reads from `Config::load()` with the original config file path. The config file path must be stored or passed to the watch context. After loading, call `config.validate()` and only apply if valid; on validation failure, log warning and retain current config (design 10.8 step 3b).
- RC-011 compliance: Every flag transition is explicitly tested:
  - `force_next_full`: set on error, cleared after successful full backup
  - `consecutive_errors`: incremented on error, reset to 0 on success
  - `reload_pending`: cleared after reload applies (via channel drain)
- `WatchContext` holds `config_path: PathBuf` for config reload.
- The `tables` filter comes from: CLI arg (standalone) or `config.watch.tables` (server mode).
- `backup::create()` schema_only=false, partitions=None, skip_check_parts_columns from config default.
- **Sync functions in async context**: `list::retention_local()` (list.rs:411) and `list::delete_local()` (list.rs:220) are both sync (`fn`). Must call via `tokio::task::spawn_blocking` to avoid blocking the async runtime. This matches the pattern used in server/routes.rs for the same functions.

---

### Task 6: Module wiring (lib.rs, main.rs standalone watch command)

**TDD Steps:**
1. Add `pub mod watch;` to `src/lib.rs`.
2. Wire `Command::Watch { .. }` in `main.rs` to:
   - Parse CLI overrides for watch_interval, full_interval, name_template, tables
   - Apply CLI overrides to config.watch fields
   - Create ChClient and S3Client
   - Query macros via `ch.get_macros()`
   - Create watch channels (shutdown, reload)
   - Spawn SIGHUP handler (Unix only) that sends on reload channel
   - Call `watch::run_watch_loop()` and handle exit
3. Verify `cargo check` passes.
4. Verify `cargo test` passes (existing tests not broken).

**Files:**
- `src/lib.rs` (modify: add `pub mod watch;`)
- `src/main.rs` (modify: wire Command::Watch to watch loop)

**Acceptance:** F007

**Notes:**
- CLI overrides follow pattern: `if let Some(v) = watch_interval { config.watch.watch_interval = v; }`
- SIGHUP handler: `#[cfg(unix)] { let mut sighup = tokio::signal::unix::signal(SignalKind::hangup()).unwrap(); tokio::spawn(async move { loop { sighup.recv().await; reload_tx.send(true).ok(); } }); }`
- Standalone watch mode also handles Ctrl+C for shutdown via `shutdown_tx`.
- The config file path is already known in main.rs from the CLI `--config` arg (or default path).

---

### Task 7: AppState watch fields + watch handle type

**TDD Steps:**
1. Write unit test `test_app_state_watch_handle_default_none` verifying `AppState.watch_handle` is `None` by default.
2. Add watch fields to `AppState`:
   ```rust
   pub watch_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
   pub watch_reload_tx: Option<tokio::sync::watch::Sender<bool>>,
   pub watch_status: Arc<Mutex<WatchStatus>>,
   ```
3. Define `WatchStatus` struct:
   ```rust
   pub struct WatchStatus {
       pub active: bool,
       pub state: String,
       pub last_full: Option<DateTime<Utc>>,
       pub last_incr: Option<DateTime<Utc>>,
       pub consecutive_errors: u32,
       pub next_backup_in: Option<Duration>,
   }
   ```
4. Update `AppState::new()` to initialize watch fields to None/default.
5. Verify test passes.

**Files:**
- `src/server/state.rs` (modify: add watch fields to AppState, add WatchStatus)

**Acceptance:** F008

**Notes:**
- `watch_shutdown_tx` and `watch_reload_tx` are `Option` because they are only created when watch is active.
- `WatchStatus` is used by the `/api/v1/watch/status` endpoint and updated by the watch loop.
- The watch loop updates `WatchStatus` via `Arc<Mutex<WatchStatus>>` shared between loop and AppState.
- Import `chrono::{DateTime, Utc}` and `std::time::Duration` in state.rs.

---

### Task 8: Server watch loop spawn + SIGHUP handler

**TDD Steps:**
1. Verify `start_server()` signature unchanged (still takes `Arc<Config>, ChClient, S3Client`).
2. Modify `start_server()` to:
   - Check `config.watch.enabled` or accept a `watch: bool` parameter
   - If watch enabled: create shutdown/reload channels, create `WatchContext`, spawn `run_watch_loop()` as background task
   - Store `shutdown_tx` and `reload_tx` in AppState
   - Add SIGHUP handler alongside existing ctrl_c handler
   - On server shutdown, send shutdown signal to watch loop
3. Update `start_server()` to accept `watch: bool` parameter (or detect from config).
4. Update the single caller in `main.rs` to pass the watch flag.
5. Verify `cargo check` passes.

**Files:**
- `src/server/mod.rs` (modify: add watch loop spawn, SIGHUP, pass watch flag)
- `src/main.rs` (modify: pass watch flag to start_server)

**Acceptance:** F009

**Notes:**
- `start_server` signature change: `pub async fn start_server(config: Arc<Config>, ch: ChClient, s3: S3Client, watch: bool) -> Result<()>`. The `watch` parameter enables the watch loop regardless of `config.watch.enabled` (CLI `--watch` flag overrides config).
- SIGHUP: `#[cfg(unix)]` guard. On non-Unix, SIGHUP is not supported (watch mode still works, just no signal-based reload -- use API endpoint instead).
- Watch loop exit handling: if `watch_is_main_process` and watch exits with error, exit the server process (design 10.9).
- `tokio::spawn` the watch loop. Store `JoinHandle` for optional await on shutdown.

---

### Task 9: Replace API stub endpoints (watch/start, watch/stop, watch/status, reload)

**TDD Steps:**
1. Implement `watch_start` handler:
   - If watch already active, return 409 Conflict
   - Create channels, spawn watch loop, store in AppState
   - Return 200 `{"status": "started"}`
2. Implement `watch_stop` handler:
   - If watch not active, return 404
   - Send shutdown signal via `watch_shutdown_tx`
   - Return 200 `{"status": "stopped"}`
3. Implement `watch_status` handler:
   - Read `WatchStatus` from AppState
   - Return JSON with state, last_full, last_incr, consecutive_errors, next_in
4. Implement `reload` handler:
   - If watch active, send reload signal via `watch_reload_tx`
   - If watch not active, just re-read config (for non-watch server mode)
   - Return 200 `{"status": "reloaded"}`
5. Update `build_router()` in `src/server/mod.rs` to use new handler functions instead of stubs.
6. Remove the stub functions from routes.rs.
7. Verify `cargo check` passes.

**Files:**
- `src/server/routes.rs` (modify: replace stubs with real implementations)
- `src/server/mod.rs` (modify: update route registration)

**Acceptance:** F010

**Notes:**
- Handler signatures follow existing pattern: `async fn handler(State(state): State<AppState>) -> Result<Json<T>, (StatusCode, Json<ErrorResponse>)>`
- `watch_start` needs access to ChClient and S3Client from AppState to create WatchContext.
- `watch_status` response matches design 10.9 example: `{"state":"sleeping","last_full":"2025-02-15T02:00:00Z","last_incr":"2025-02-15T03:00:00Z","next_in":"47m"}`
- `reload` endpoint maps to SIGHUP behavior (design 10.8): `POST /api/v1/reload`.
- The `restart_stub` is NOT replaced in this plan (out of scope).

---

### Task 10: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/server, src/clickhouse, src/watch (create)

**TDD Steps:**

1. **Create `src/watch/CLAUDE.md`:**
   - Use CLAUDE.md template for new modules
   - Document: WatchState enum, WatchContext struct, run_watch_loop(), resolve_name_template(), resume_state(), ResumeDecision enum
   - Auto-generate directory tree

2. **Update `src/server/CLAUDE.md`:**
   - Add watch integration patterns (WatchStatus, watch channels in AppState)
   - Document replaced stub endpoints
   - Document SIGHUP handler
   - Update directory tree

3. **Update `src/clickhouse/CLAUDE.md`:**
   - Add `get_macros()` method to Public API section
   - Add `MacroRow` to Row Types section

4. **Validate all CLAUDE.md files have required sections:**
   - Parent Context
   - Directory Structure
   - Key Patterns
   - Parent Rules

**Files:**
- `src/watch/CLAUDE.md` (create)
- `src/server/CLAUDE.md` (update)
- `src/clickhouse/CLAUDE.md` (update)

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All API signatures verified via grep against source (knowledge_graph.json) |
| RC-011 | PASS | State flags (`force_next_full`, `consecutive_errors`, `reload` channel) all have explicit set/clear paths with tests in Task 5 |
| RC-015 | PASS | Cross-task data flows verified: Task 3 returns String, Task 4 returns ResumeDecision, Task 5 consumes both correctly |
| RC-016 | PASS | WatchContext struct (Task 5) lists all fields consumed by run_watch_loop; WatchStatus struct (Task 7) lists all fields consumed by Task 9 endpoints |
| RC-017 | PASS | All `self.X` fields in Task 5 code are declared in WatchContext (same task). All AppState fields in Task 9 are declared in Task 7. |
| RC-018 | PASS | Every task has named test functions with specific assertions |
| RC-019 | PASS | `get_macros()` follows `get_disks()` pattern exactly. Route handlers follow `create_remote` pattern. |
| RC-021 | PASS | File locations verified: WatchConfig at config.rs:392, AppState at state.rs:26, Metrics at metrics.rs:18, parse_duration_secs at config.rs:1248 |
| RC-008 | PASS | Task ordering verified: Task 1 (pub fn) before Task 3 (uses it), Task 2 (get_macros) before Task 5 (calls it), Task 7 (AppState fields) before Task 9 (uses them) |

## Notes

### Phase 4.5 Skip Justification
Interface skeleton simulation is skipped because:
- All existing APIs are verified against source via knowledge_graph.json
- New code is in a new module (`src/watch/`) with no complex type dependencies on existing code beyond well-known types (Config, ChClient, S3Client, BackupSummary)
- The new `get_macros()` method follows an identical pattern to `get_disks()` which compiles successfully

### Watch State Metric Encoding
The `watch_state` IntGauge uses numeric values:
- 0 = inactive (watch not running)
- 1 = idle (between cycles)
- 2 = creating full
- 3 = creating incremental
- 4 = uploading
- 5 = cleaning/retention
- 6 = sleeping
- 7 = error/backoff

### Config File Path for Reload
The config file path must be threaded through to `WatchContext` for hot-reload. In standalone mode, it comes from CLI `--config` arg. In server mode, it comes from the same source. The path is stored as `config_path: PathBuf` in `WatchContext`.

### Tables Filter Resolution
Table filter priority: CLI `--tables` > `config.watch.tables` > `config.backup.tables` > None (match all). This is resolved once at watch loop start and on config reload.
