# Type Verification -- Symbols

## Dead Code Items Verified

| Item | Location | Type/Kind | Status | Evidence |
|------|----------|-----------|--------|----------|
| `ChClient::debug` | `src/clickhouse/client.rs:24` | `bool` field | DEAD -- stored but never read after construction | `#[allow(dead_code)]` annotation; grep for `self.debug` / `.debug` shows only constructor writes |
| `ChClient::inner()` | `src/clickhouse/client.rs:252` | `pub fn inner(&self) -> &clickhouse::Client` | DEAD -- zero callers | grep `\.inner()` returns 0 hits outside definition |
| `S3Client::inner()` | `src/storage/s3.rs:220` | `pub fn inner(&self) -> &aws_sdk_s3::Client` | DEAD -- zero callers | grep `\.inner()` returns 0 hits outside definition |
| `S3Client::concurrency()` | `src/storage/s3.rs:235` | `pub fn concurrency(&self) -> u32` | DEAD -- zero callers | grep `\.concurrency()` returns 0 hits |
| `S3Client::object_disk_path()` | `src/storage/s3.rs:243` | `pub fn object_disk_path(&self) -> &str` | DEAD -- zero callers | grep `\.object_disk_path()` returns 0 hits |
| `S3Client::concurrency` field | `src/storage/s3.rs:58` | `u32` | DEAD -- only read by dead `concurrency()` getter | `self.concurrency` only appears in the getter |
| `S3Client::object_disk_path` field | `src/storage/s3.rs:60` | `String` | DEAD -- only read by dead `object_disk_path()` getter | `self.object_disk_path` only appears in the getter |
| `attach_parts()` | `src/restore/attach.rs:487` | `pub async fn attach_parts(params: &AttachParams<'_>) -> Result<u64>` | DEAD -- zero callers | `#[allow(dead_code)]`; only `attach_parts_owned` is used |
| `ProgressTracker::disabled()` | `src/progress.rs:48` | `pub fn disabled() -> Self` | TEST-ONLY -- only called from `#[cfg(test)]` | grep shows 2 uses, both in `progress.rs` tests |
| `ProgressTracker::is_active()` | `src/progress.rs:67` | `pub fn is_active(&self) -> bool` | TEST-ONLY -- only called from `#[cfg(test)]` in progress.rs | Non-test `.is_active()` calls are on `RemapConfig`, not `ProgressTracker` |
| `ActionLog::running()` | `src/server/actions.rs:118` | `pub fn running(&self) -> Option<&ActionEntry>` | TEST-ONLY -- only called from `#[cfg(test)]` blocks | Production code uses `running_ops` HashMap instead |
| `Metrics::parts_uploaded_total` | `src/server/metrics.rs:32` | `IntCounter` | UNUSED-METRIC -- registered but never incremented in prod | grep shows no `.inc()` or `.inc_by()` calls outside metrics.rs |
| `Metrics::parts_skipped_incremental_total` | `src/server/metrics.rs:35` | `IntCounter` | UNUSED-METRIC -- registered but never incremented in prod | grep shows no `.inc()` or `.inc_by()` calls outside metrics.rs |
| `ListParams::format` | `src/server/routes.rs:74` | `Option<String>` | INTENTIONALLY UNUSED -- deserialized from query params but stored for integration table DDL compatibility | Documented in comment |

## Arc/Mutex/ArcSwap Analysis

| Field | Location | Wrapper | Reason | Verdict |
|-------|----------|---------|--------|---------|
| `AppState::config` | `src/server/state.rs:71` | `Arc<ArcSwap<Config>>` | Hot-swapped via `/api/v1/restart` and `/api/v1/reload` | REQUIRED -- both `.load()` and `.store()` are used |
| `AppState::ch` | `src/server/state.rs:72` | `Arc<ArcSwap<ChClient>>` | Hot-swapped via restart/reload | REQUIRED |
| `AppState::s3` | `src/server/state.rs:73` | `Arc<ArcSwap<S3Client>>` | Hot-swapped via restart/reload | REQUIRED |
| `AppState::action_log` | `src/server/state.rs:74` | `Arc<Mutex<ActionLog>>` | Mutated by multiple async tasks (try_start_op, finish_op, fail_op, kill_op, routes) | REQUIRED |
| `AppState::running_ops` | `src/server/state.rs:75` | `Arc<Mutex<HashMap<...>>>` | Mutated by multiple async tasks | REQUIRED |
| `AppState::op_semaphore` | `src/server/state.rs:76` | `Arc<Semaphore>` | Shared across handlers for concurrency control | REQUIRED |
| `AppState::metrics` | `src/server/state.rs:78` | `Option<Arc<Metrics>>` | Read-only after creation, shared across handlers | REQUIRED |
| `AppState::watch_shutdown_tx` | `src/server/state.rs:82` | `Arc<Mutex<Option<...>>>` | Written by `spawn_watch_from_state`, read by route handlers | REQUIRED -- documented in comment |
| `AppState::watch_reload_tx` | `src/server/state.rs:84` | `Arc<Mutex<Option<...>>>` | Same pattern as watch_shutdown_tx | REQUIRED |
| `AppState::watch_status` | `src/server/state.rs:86` | `Arc<Mutex<WatchStatus>>` | Written by watch loop, read by API handlers | REQUIRED |
| `AppState::manifest_cache` | `src/server/state.rs:91` | `Arc<Mutex<ManifestCache>>` | Written by list_remote_cached, invalidated by mutating ops | REQUIRED |
| `WatchContext::manifest_cache` | `src/watch/mod.rs:254` | `Option<Arc<Mutex<ManifestCache>>>` | Shared with server AppState for cache invalidation | REQUIRED |

## ChBackupError Variant Usage

| Variant | Constructed in prod | Matched in exit_code() | Verdict |
|---------|-------------------|----------------------|---------|
| `ClickHouseError(String)` | NO | NO (falls through to `_ => 1`) | UNUSED outside tests |
| `S3Error(String)` | NO | NO | UNUSED outside tests |
| `ConfigError(String)` | NO | NO | UNUSED outside tests |
| `LockError(String)` | YES (lock.rs) | YES (code 4) | USED |
| `BackupError(String)` | NO | YES (code 3 when "not found") | PARTIAL -- matched but never constructed |
| `RestoreError(String)` | NO | NO | UNUSED outside tests |
| `ManifestError(String)` | NO | YES (code 3 when "not found") | PARTIAL -- matched but never constructed |
| `IoError(io::Error)` | IMPLICIT (#[from]) | NO | USED via From trait |

## Test-Only Helper Construction Sites

When removing S3Client fields `concurrency` and `object_disk_path`, the following test construction site must also be updated:
- `src/storage/s3.rs:1558-1568` -- test helper constructs S3Client directly with struct literal including `concurrency: 1` and `object_disk_path: String::new()`
