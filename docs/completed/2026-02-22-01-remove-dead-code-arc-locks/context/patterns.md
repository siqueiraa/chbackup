# Pattern Discovery

## Global Patterns
No `docs/patterns/` directory exists in this project. Patterns discovered locally.

## Relevant Patterns for Dead Code Removal

### 1. Zero Warnings Policy
The codebase enforces zero warnings. Dead code that the compiler can see is suppressed via `#[allow(dead_code)]` annotations. There are exactly 2 such annotations:
- `src/clickhouse/client.rs:23` -- `debug` field on `ChClient` struct
- `src/restore/attach.rs:486` -- `attach_parts()` function (borrowed-params variant)

### 2. Public API Accessor Pattern
Several modules expose `pub fn` getters that were added for "future use" or "Phase 6 completeness" but are never actually called:
- `ChClient::inner()` -- returns underlying `clickhouse::Client`
- `S3Client::inner()` -- returns underlying `aws_sdk_s3::Client`
- `S3Client::concurrency()` -- returns configured concurrency
- `S3Client::object_disk_path()` -- returns object disk path
- `ProgressTracker::disabled()` -- constructor (only used in tests)
- `ProgressTracker::is_active()` -- getter (only used in tests)
- `ActionLog::running()` -- finder (only used in tests)

### 3. AppState Wrapping Pattern
All `AppState` fields use `Arc<...>` wrapping because axum's `State<T>` requires `Clone`. This is architecturally required:
- `Arc<ArcSwap<T>>` for config/ch/s3 -- needed for hot-swap via `/api/v1/restart` and `/api/v1/reload`
- `Arc<Mutex<T>>` for action_log, running_ops, watch_shutdown_tx, watch_reload_tx, watch_status, manifest_cache -- needed because these are mutated from multiple async handler tasks
- `Arc<Semaphore>` for op_semaphore -- needed for shared concurrency control

### 4. Metrics Registration Pattern
All 14 metric families are registered at Metrics::new() time, even if they're never incremented:
- `parts_uploaded_total` and `parts_skipped_incremental_total` are registered but never incremented in production code
- This is for Prometheus convention (metrics should appear even at zero)

### 5. Error Variant Pattern
`ChBackupError` has 7 variants but only `LockError` and `IoError` (via #[from]) are constructed in production. Other variants exist for exit code mapping and future use.

## Pattern Applicability to This Plan
- Pattern 2 (unused public APIs) is the primary target for removal
- Pattern 1 (allow(dead_code)) items should be evaluated for removal
- Pattern 4 (unused metrics) is a judgment call -- removing metrics breaks Prometheus expectations
- Pattern 5 (unused error variants) is a judgment call -- they define the error taxonomy
- Pattern 3 (Arc wrapping) is architecturally required and should NOT be changed
