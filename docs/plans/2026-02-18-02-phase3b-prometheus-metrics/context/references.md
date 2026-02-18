# Symbol and Reference Analysis

**Date**: 2026-02-18
**Phase**: 3b -- Prometheus Metrics

## Key Symbols to Modify

### 1. `metrics_stub` (routes.rs:900)

**Current definition:**
```rust
pub async fn metrics_stub() -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "not implemented (Phase 3b)")
}
```

**References (3 total, 2 files):**
- `src/server/mod.rs:82` -- Route registration: `.route("/metrics", get(routes::metrics_stub))`
- `src/server/routes.rs:1109-1111` -- Test: `test_stub_endpoints_return_501`

**Action**: Replace `metrics_stub` with real `metrics` handler. Update route registration and test.

### 2. `AppState` (state.rs:23)

**Current fields:**
```rust
pub struct AppState {
    pub config: Arc<Config>,
    pub ch: ChClient,
    pub s3: S3Client,
    pub action_log: Arc<Mutex<ActionLog>>,
    pub current_op: Arc<Mutex<Option<RunningOp>>>,
    pub op_semaphore: Arc<Semaphore>,
}
```

**References**: 27 references across 4 files:
- `src/server/routes.rs` -- All handler functions use `State(state): State<AppState>`
- `src/server/state.rs` -- Struct definition, impl block, auto_resume
- `src/server/auth.rs` -- auth_middleware uses `State<AppState>`
- `src/server/mod.rs` -- build_router, start_server

**Action**: Add a `metrics: Arc<Metrics>` field (or similar) to hold the prometheus Registry/metric instances. Constructor `AppState::new()` must initialize it.

### 3. `AppState::new()` (state.rs:42)

**Current signature:**
```rust
pub fn new(config: Arc<Config>, ch: ChClient, s3: S3Client) -> Self
```

**Action**: Will need to create Metrics struct and pass it to AppState during construction.

### 4. `finish_op` (state.rs:99)

**Incoming calls (11 callers):**
- `routes.rs`: post_actions, create_backup, upload_backup, download_backup, restore_backup, create_remote, restore_remote, delete_backup, clean_remote_broken, clean_local_broken
- `state.rs`: auto_resume (upload, download, restore branches)

**Action**: Potential instrumentation point for `chbackup_backup_duration_seconds`, `chbackup_in_progress` gauge decrement, `successful_backups_total` counter.

### 5. `fail_op` (state.rs:113)

**Incoming calls (10 callers):**
- `routes.rs`: create_backup, upload_backup, download_backup, restore_backup, create_remote (x2), restore_remote (x2), delete_backup, clean_remote_broken, clean_local_broken
- `state.rs`: auto_resume (upload, download, restore branches)

**Action**: Instrumentation point for `chbackup_errors_total` counter, `chbackup_in_progress` gauge decrement.

### 6. `try_start_op` (state.rs:65)

**Action**: Instrumentation point for `chbackup_in_progress` gauge set to 1.

### 7. `enable_metrics` (config.rs:432)

**References (3 in config.rs):**
- Line 432: Field definition
- Line 614: Default value in `impl Default for ApiConfig`
- Lines 1133-1134: Env overlay

**Action**: Check this field in the metrics handler -- if false, return empty response or 501. Also used to conditionally gate metric recording.

### 8. `build_router` (mod.rs:33)

**Signature**: `pub fn build_router(state: AppState) -> Router`

**Action**: Change route from `metrics_stub` to real `metrics` handler. The handler needs `State(state)` to access the metrics registry.

## Files That Will Be Modified

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `prometheus = "0.13"` dependency |
| `src/server/mod.rs` | Update route from metrics_stub to metrics handler, add `metrics` module |
| `src/server/state.rs` | Add `metrics` field to `AppState`, initialize in `new()` |
| `src/server/routes.rs` | Replace `metrics_stub` with real `metrics` handler, update test |
| `src/lib.rs` | No change needed (server module already declared) |

## New Files

| File | Purpose |
|------|---------|
| `src/server/metrics.rs` | Metrics struct with prometheus Registry, all metric definitions, helper methods |

## Metric Recording Points (from call hierarchy analysis)

### Per-operation endpoints in routes.rs

Each operation handler follows this pattern:
```
try_start_op -> tokio::spawn { ... match result { Ok => finish_op, Err => fail_op } }
```

The instrumentation must happen at:
1. **try_start_op**: Set `in_progress` gauge to 1
2. **finish_op**: Record duration histogram, increment success counter, set `in_progress` to 0
3. **fail_op**: Increment error counter, set `in_progress` to 0

### Specific metrics and their recording points:

| Metric | Where to Record | Source Data |
|--------|----------------|-------------|
| `chbackup_backup_duration_seconds` | finish_op / fail_op | Compute from ActionEntry.start to now() |
| `chbackup_backup_size_bytes` | create_backup handler (after create() returns Ok) | From BackupManifest.compressed_size |
| `chbackup_backup_last_success_timestamp` | finish_op (for create/upload operations) | Utc::now() |
| `chbackup_parts_uploaded_total` | upload handler after success | Count from manifest tables |
| `chbackup_parts_skipped_incremental_total` | upload handler after diff | Count carried parts |
| `chbackup_errors_total` | fail_op | Per operation label |
| `chbackup_number_backups_remote` | metrics handler (lazy query) | list_remote count |
| `chbackup_number_backups_local` | metrics handler (lazy query) | list_local count |
| `chbackup_in_progress` | try_start_op / finish_op / fail_op | 1/0 toggle |
| `chbackup_watch_state` | Deferred to Phase 3d | N/A |
| `chbackup_watch_last_full_timestamp` | Deferred to Phase 3d | N/A |
| `chbackup_watch_consecutive_errors` | Deferred to Phase 3d | N/A |

### Watch-related metrics (3 of 12)

Three metrics (`watch_state`, `watch_last_full_timestamp`, `watch_consecutive_errors`) are watch-mode specific. Watch mode is Phase 3d, which comes AFTER Phase 3b. These metric definitions should be created now (register the gauges) but will only be updated when watch mode is implemented. They will report default/zero values until then.
