# Plan: Phase 3b -- Prometheus Metrics

## Goal

Add Prometheus metrics to the chbackup HTTP API server, replacing the `/metrics` stub endpoint with a real Prometheus text exposition handler. Implements all metrics from design doc section 9 (line 1833) and roadmap Phase 3b table.

## Architecture Overview

A new `src/server/metrics.rs` module defines a `Metrics` struct holding a custom `prometheus::Registry` and all metric instances. The struct is stored in `AppState` as `Option<Arc<Metrics>>` (None when `enable_metrics=false`). The existing operation handler pattern in `routes.rs` is extended to record duration, success/failure counters, and size gauges in the spawned task match arms. Backup count gauges are refreshed lazily on each `/metrics` scrape. Watch-related metrics are registered but report defaults until Phase 3d.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Metrics struct**: Created in `AppState::new()`, stored by `AppState`, accessed by route handlers and metrics endpoint
- **Registry**: Owned by `Metrics` struct (custom, not global), used only during `/metrics` text encoding
- **Operation instrumentation**: Happens within existing `tokio::spawn` match arms in `routes.rs`
- **Backup count gauges**: Updated at scrape time in `metrics` handler, using `list_local()` (via `spawn_blocking`) and `list_remote()` (async)

### What This Plan CANNOT Do
- Cannot implement watch-related metric updates (Phase 3d dependency) -- metrics are registered but static
- Cannot add `s3_copy_object_total` metric accurately without modifying S3Client internals -- deferred to Phase 4 (tracked in Known Related Issues)
- Cannot test with real Prometheus scraper (no integration test infrastructure for Prometheus)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| prometheus crate API mismatch | GREEN | API verified via crate docs (v0.13, stable API since v0.10) |
| Blocking `/metrics` handler | GREEN | `list_local()` via `spawn_blocking`, `list_remote()` is async. Total scrape time bounded by S3 ListObjects latency (~100ms-1s) |
| AppState breaking change | GREEN | Adding one field with Option wrapper; constructor signature unchanged |
| Watch metrics report stale 0 | GREEN | Expected behavior -- documented as Phase 3d dependency |
| Metric name conflicts | GREEN | All prefixed with `chbackup_`, no collision with existing code |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `INFO.*Metrics registry created` | yes | Logged when `enable_metrics=true` during AppState construction |
| `WARN.*Failed to refresh backup counts` | no (situational) | Logged if list_local/list_remote fails during scrape |
| `ERROR.*` | no (forbidden) | No errors should appear from metrics code |
| `DEBUG_VERIFY:F001_metrics_registered` | yes | Confirms metrics struct is created in AppState |
| `DEBUG_VERIFY:F002_scrape_ok` | yes | Confirms /metrics endpoint returns 200 with prometheus text |
| `DEBUG_VERIFY:F003_duration_recorded` | yes | Confirms operation duration was observed |
| `DEBUG_VERIFY:F004_error_counted` | yes | Confirms error counter was incremented |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| `s3_copy_object_total` counter | Requires adding counter inside S3Client methods (different module) | Phase 4 or dedicated S3 instrumentation plan |
| `watch_last_incremental_timestamp` | Watch mode not implemented | Phase 3d |
| Watch metric updates | No watch loop to update them | Phase 3d |
| `restore_duration_seconds` separate metric | Covered by `backup_duration_seconds` with `operation="restore"` label | Already handled via HistogramVec labels |
| `successful_backups_total` / `failed_backups_total` | Covered by separate success/error counters with operation labels | Design doc lists both; plan uses `successful_operations_total` + `errors_total` with `operation` label per roadmap |

## Dependency Groups

```
Group A (Sequential -- Metrics Foundation):
  - Task 1: Add prometheus dependency to Cargo.toml
  - Task 2: Create Metrics struct in src/server/metrics.rs (depends on Task 1)
  - Task 3: Add metrics field to AppState in state.rs (depends on Task 2)

Group B (Sequential -- Endpoint + Instrumentation, depends on Group A):
  - Task 4: Replace metrics_stub with real /metrics handler
  - Task 5: Instrument operation handlers with duration/success/failure metrics (depends on Task 4 for pattern)

Group C (Final -- Documentation, depends on Group B):
  - Task 6: Update CLAUDE.md for src/server module
```

## Tasks

### Task 1: Add prometheus dependency to Cargo.toml

**TDD Steps:**
1. Add `prometheus = "0.13"` to `[dependencies]` section of `Cargo.toml`
2. Run `cargo check` to verify dependency resolves
3. Verify with a compile-time import test

**Files:** `Cargo.toml`, `src/lib.rs`
**Acceptance:** F001

**Implementation Notes:**
- Add after the `# HTTP API server` comment block in Cargo.toml (line ~72)
- Add a compile-time test in `src/lib.rs` similar to existing `test_phase3a_deps_available`:
  ```rust
  #[test]
  fn test_phase3b_prometheus_available() {
      use prometheus::{Registry, TextEncoder, Encoder, IntCounter, Gauge, Histogram, HistogramOpts, opts};
      let registry = Registry::new();
      let counter = IntCounter::new("test_counter", "help").unwrap();
      registry.register(Box::new(counter.clone())).unwrap();
      let encoder = TextEncoder::new();
      let mut buffer = Vec::new();
      encoder.encode(&registry.gather(), &mut buffer).unwrap();
      assert!(!buffer.is_empty());
  }
  ```

### Task 2: Create Metrics struct in src/server/metrics.rs

**TDD Steps:**
1. Write unit test `test_metrics_new_registers_all`: create `Metrics::new()`, verify all expected metric families are registered in the registry
2. Write unit test `test_metrics_encode_text`: create `Metrics`, call `encode()`, verify output contains expected metric names
3. Write unit test `test_metrics_counter_increment`: create `Metrics`, increment `errors_total` counter, verify encoded output shows count 1
4. Implement `Metrics` struct with all metric fields and `new()` constructor
5. Implement `encode()` method returning String (text/plain prometheus format)
6. Run tests, verify all pass

**Files:** `src/server/metrics.rs` (new), `src/server/mod.rs` (add `pub mod metrics;`)
**Acceptance:** F001, F002

**Implementation Notes:**

The `Metrics` struct holds all prometheus metric instances:

```rust
use prometheus::{
    Encoder, Gauge, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Registry,
    TextEncoder, opts,
};

pub struct Metrics {
    pub registry: Registry,

    // Per-operation histogram (labels: operation = create|upload|download|restore)
    pub backup_duration_seconds: HistogramVec,

    // Last backup compressed size
    pub backup_size_bytes: Gauge,

    // Unix timestamp of last successful create/upload
    pub backup_last_success_timestamp: Gauge,

    // Parts uploaded (cumulative counter)
    pub parts_uploaded_total: IntCounter,

    // Parts skipped via diff-from (cumulative counter)
    pub parts_skipped_incremental_total: IntCounter,

    // Errors per operation type (labels: operation)
    pub errors_total: IntCounterVec,

    // Successful operations per type (labels: operation)
    pub successful_operations_total: IntCounterVec,

    // Current backup counts (refreshed on scrape)
    pub number_backups_local: IntGauge,
    pub number_backups_remote: IntGauge,

    // 1 if operation running, 0 otherwise
    pub in_progress: IntGauge,

    // Watch-related gauges (registered, set to 0 until Phase 3d)
    pub watch_state: IntGauge,
    pub watch_last_full_timestamp: Gauge,
    pub watch_last_incremental_timestamp: Gauge,
    pub watch_consecutive_errors: IntGauge,
}
```

Constructor pattern:
```rust
impl Metrics {
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        let duration = HistogramVec::new(
            HistogramOpts::new(
                "chbackup_backup_duration_seconds",
                "Duration of backup operations in seconds",
            )
            .buckets(vec![1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0]),
            &["operation"],
        )?;
        registry.register(Box::new(duration.clone()))?;

        // ... register all metrics similarly ...

        Ok(Self { registry, backup_duration_seconds: duration, /* ... */ })
    }

    pub fn encode(&self) -> Result<String, prometheus::Error> {
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&families, &mut buffer)?;
        Ok(String::from_utf8(buffer).unwrap_or_default())
    }
}
```

All metric names follow roadmap table:
- `chbackup_backup_duration_seconds` -- HistogramVec with `operation` label
- `chbackup_backup_size_bytes` -- Gauge
- `chbackup_backup_last_success_timestamp` -- Gauge
- `chbackup_parts_uploaded_total` -- IntCounter
- `chbackup_parts_skipped_incremental_total` -- IntCounter
- `chbackup_errors_total` -- IntCounterVec with `operation` label
- `chbackup_successful_operations_total` -- IntCounterVec with `operation` label
- `chbackup_number_backups_local` -- IntGauge
- `chbackup_number_backups_remote` -- IntGauge
- `chbackup_in_progress` -- IntGauge
- `chbackup_watch_state` -- IntGauge
- `chbackup_watch_last_full_timestamp` -- Gauge
- `chbackup_watch_last_incremental_timestamp` -- Gauge
- `chbackup_watch_consecutive_errors` -- IntGauge

### Task 3: Add metrics field to AppState

**TDD Steps:**
1. Write unit test `test_app_state_with_metrics_enabled`: create AppState with metrics enabled, verify `state.metrics.is_some()`
2. Write unit test `test_app_state_with_metrics_disabled`: set `enable_metrics=false`, create AppState, verify `state.metrics.is_none()`
3. Add `pub metrics: Option<Arc<Metrics>>` field to `AppState` struct
4. Update `AppState::new()` to conditionally create Metrics based on `config.api.enable_metrics`
5. Add debug marker: `info!("DEBUG_VERIFY:F001_metrics_registered count={}", ...)` when metrics created
6. Run tests, verify all pass (including existing tests that construct AppState directly)

**Files:** `src/server/state.rs`
**Acceptance:** F001

**Implementation Notes:**

In `state.rs`, add import:
```rust
use super::metrics::Metrics;
```

Add field to AppState:
```rust
pub struct AppState {
    pub config: Arc<Config>,
    pub ch: ChClient,
    pub s3: S3Client,
    pub action_log: Arc<Mutex<ActionLog>>,
    pub current_op: Arc<Mutex<Option<RunningOp>>>,
    pub op_semaphore: Arc<Semaphore>,
    pub metrics: Option<Arc<Metrics>>,  // None when enable_metrics=false
}
```

Update `AppState::new()`:
```rust
let metrics = if config.api.enable_metrics {
    match Metrics::new() {
        Ok(m) => {
            let count = m.registry.gather().len();
            info!("DEBUG_VERIFY:F001_metrics_registered count={}", count);
            info!("Metrics registry created with {} metric families", count);
            Some(Arc::new(m))
        }
        Err(e) => {
            warn!(error = %e, "Failed to create metrics registry, continuing without metrics");
            None
        }
    }
} else {
    None
};
```

The existing tests in `state.rs` that test semaphore/action_log behavior directly (without constructing full AppState) will NOT be affected because they test those components independently.

### Task 4: Replace metrics_stub with real /metrics handler

**TDD Steps:**
1. Write unit test `test_metrics_handler_returns_prometheus_text`: verify the handler returns text/plain with prometheus format when metrics enabled
2. Write unit test `test_metrics_handler_disabled`: verify the handler returns 501 when metrics is None (enable_metrics=false)
3. Remove `metrics_stub()` function from `routes.rs`
4. Add `metrics()` handler to `routes.rs` that encodes prometheus text from `state.metrics`
5. Update route in `mod.rs`: change `routes::metrics_stub` to `routes::metrics`
6. Update `test_stub_endpoints_return_501` to remove the metrics_stub assertion
7. Add debug marker: `info!("DEBUG_VERIFY:F002_scrape_ok")` in the success path
8. Run tests, verify `metrics_stub` is gone and new handler works

**Files:** `src/server/routes.rs`, `src/server/mod.rs`
**Acceptance:** F002

**Implementation Notes:**

Handler in routes.rs:
```rust
use axum::response::IntoResponse;

/// GET /metrics -- Prometheus metrics endpoint
pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let Some(metrics) = &state.metrics else {
        return (StatusCode::NOT_IMPLEMENTED, "metrics disabled".to_string());
    };

    // Refresh backup count gauges (expensive -- OK for 15-30s scrape intervals)
    refresh_backup_counts(&state, metrics).await;

    match metrics.encode() {
        Ok(text) => {
            info!("DEBUG_VERIFY:F002_scrape_ok");
            (StatusCode::OK, text)
        }
        Err(e) => {
            warn!(error = %e, "Failed to encode metrics");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("metrics encoding error: {}", e))
        }
    }
}

async fn refresh_backup_counts(state: &AppState, metrics: &Metrics) {
    // Refresh local backup count (sync function -- use spawn_blocking)
    let data_path = state.config.clickhouse.data_path.clone();
    match tokio::task::spawn_blocking(move || crate::list::list_local(&data_path)).await {
        Ok(Ok(summaries)) => metrics.number_backups_local.set(summaries.len() as i64),
        Ok(Err(e)) => warn!(error = %e, "Failed to refresh local backup count for metrics"),
        Err(e) => warn!(error = %e, "spawn_blocking failed for list_local in metrics"),
    }

    // Refresh remote backup count (async)
    match crate::list::list_remote(&state.s3).await {
        Ok(summaries) => metrics.number_backups_remote.set(summaries.len() as i64),
        Err(e) => warn!(error = %e, "Failed to refresh remote backup count for metrics"),
    }

    // Refresh in_progress gauge from current_op
    let is_running = state.current_op.lock().await.is_some();
    metrics.in_progress.set(if is_running { 1 } else { 0 });
}
```

Route registration in mod.rs line 83:
```rust
// Change from:
.route("/metrics", get(routes::metrics_stub))
// To:
.route("/metrics", get(routes::metrics))
```

Update test in routes.rs:
- Remove the metrics_stub lines from `test_stub_endpoints_return_501`

### Task 5: Instrument operation handlers with metrics

**TDD Steps:**
1. Write unit test `test_metrics_duration_observation`: create Metrics, observe a duration, verify histogram count is 1
2. Write unit test `test_metrics_error_increment`: create Metrics, increment error counter for "create", verify count is 1
3. Write unit test `test_metrics_success_increment`: create Metrics, increment success counter, verify
4. Write unit test `test_metrics_size_gauge`: create Metrics, set backup_size_bytes, verify value
5. Instrument each spawned task in routes.rs operation handlers:
   - Add `Instant::now()` before the operation call
   - On success: observe duration histogram, increment success counter, set last_success_timestamp
   - On failure: observe duration histogram, increment error counter
   - For create: set backup_size_bytes from manifest
6. Add debug markers in instrumented paths
7. Run all tests

**Files:** `src/server/routes.rs`
**Acceptance:** F003, F004

**Implementation Notes:**

The instrumentation pattern is added inside each spawned task's match arms. Example for `create_backup`:

```rust
tokio::spawn(async move {
    let start_time = std::time::Instant::now();
    // ... existing code ...
    let result = crate::backup::create(/* ... */).await;
    let duration = start_time.elapsed().as_secs_f64();

    match result {
        Ok(manifest) => {
            if let Some(m) = &state_clone.metrics {
                m.backup_duration_seconds.with_label_values(&["create"]).observe(duration);
                m.successful_operations_total.with_label_values(&["create"]).inc();
                m.backup_last_success_timestamp.set(chrono::Utc::now().timestamp() as f64);
                m.backup_size_bytes.set(manifest.compressed_size as f64);
                info!("DEBUG_VERIFY:F003_duration_recorded op=create duration={}", duration);
            }
            info!(backup_name = %backup_name, "Create operation completed");
            state_clone.finish_op(id).await;
        }
        Err(e) => {
            if let Some(m) = &state_clone.metrics {
                m.backup_duration_seconds.with_label_values(&["create"]).observe(duration);
                m.errors_total.with_label_values(&["create"]).inc();
                info!("DEBUG_VERIFY:F004_error_counted op=create");
            }
            warn!(backup_name = %backup_name, error = %e, "Create operation failed");
            state_clone.fail_op(id, e.to_string()).await;
        }
    }
});
```

Handlers to instrument (with operation label):
- `create_backup` -- operation="create", set `backup_size_bytes` from manifest.compressed_size
- `upload_backup` -- operation="upload"
- `download_backup` -- operation="download"
- `restore_backup` -- operation="restore"
- `create_remote` -- operation="create_remote" (compound: uses create step result for size)
- `restore_remote` -- operation="restore_remote"
- `delete_backup` -- operation="delete"
- `clean_remote_broken` -- operation="clean_broken_remote"
- `clean_local_broken` -- operation="clean_broken_local"

For `create_backup`, the `Ok` branch receives the `BackupManifest`:
```rust
Ok(manifest) => {
    // manifest is BackupManifest with compressed_size: u64
    m.backup_size_bytes.set(manifest.compressed_size as f64);
}
```

**Note on create_backup return type change:** Currently `create_backup` calls `crate::backup::create()` which returns `Result<BackupManifest>`. The existing code uses `Ok(_)` to discard the manifest. We need to bind it: `Ok(manifest)` to access `manifest.compressed_size`. This is a safe change since the variable just goes from `_` to a named binding.

For `upload_backup`, parts counting is deferred (requires changes to upload module internals to return counts). The counter metrics (`parts_uploaded_total`, `parts_skipped_incremental_total`) will be registered but only incremented when the upload module is extended to report part counts (can be done in same task if straightforward, otherwise document as limitation).

### Task 6: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/server

**TDD Steps:**

1. Regenerate directory tree for src/server:
   ```bash
   tree -L 2 src/server --noreport 2>/dev/null || ls -la src/server
   ```

2. Update `src/server/CLAUDE.md`:
   - Add `metrics.rs` to Directory Structure
   - Add `metrics: Option<Arc<Metrics>>` to AppState fields documentation
   - Add "Prometheus Metrics" section to Key Patterns
   - Remove `/metrics` from Stub Endpoints list
   - Add metrics.rs API to Public API section

3. Validate:
   ```bash
   grep -q "Parent Context" src/server/CLAUDE.md
   grep -q "Directory Structure" src/server/CLAUDE.md
   grep -q "Key Patterns" src/server/CLAUDE.md
   grep -q "Parent Rules" src/server/CLAUDE.md
   grep -q "metrics.rs" src/server/CLAUDE.md
   ```

**Files:** `src/server/CLAUDE.md`
**Acceptance:** FDOC

## Notes

### Phase 4.5 Skip Justification
Interface skeleton simulation is skipped because:
- The `prometheus` crate is not yet in Cargo.toml so no stub file can compile
- All changes use well-documented prometheus crate APIs (v0.13, stable)
- All internal types/imports are verified via knowledge_graph.json
- Compilation is verified at Task 1 level (adding dependency + compile test)

### Metrics Not In Roadmap Table But In Design Doc
The design doc (line 1833) lists some metrics not in the roadmap Phase 3b table:
- `restore_duration_seconds` -- covered by `backup_duration_seconds` HistogramVec with `operation="restore"` label
- `s3_copy_object_total` -- deferred (requires S3Client instrumentation)
- `successful_backups_total` / `failed_backups_total` -- implemented as `successful_operations_total` / `errors_total` with `operation` label
- `in_progress_commands` -- implemented as `in_progress` IntGauge
- `watch_last_incremental_timestamp` -- registered, defaults to 0

### prometheus crate version
The roadmap specifies v0.13 and the diagnostics note v0.14 is available. Using `prometheus = "0.13"` as specified in the roadmap's Cargo.toml section (line 564).

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (cross-task types) | PASS | Metrics struct flows from Task 2 -> Task 3 (AppState) -> Tasks 4,5 (handlers). Types consistent. |
| RC-016 (struct field completeness) | PASS | All Metrics fields used in Tasks 4-5 are defined in Task 2 |
| RC-017 (acceptance IDs match) | PASS | F001 (Tasks 1-3), F002 (Task 4), F003 (Task 5 success), F004 (Task 5 error), FDOC (Task 6) |
| RC-018 (dependencies satisfied) | PASS | Group A sequential, Group B depends on A, Group C depends on B |
| RC-006 (API verification) | PASS | prometheus crate APIs verified via crate docs. Internal APIs verified via knowledge_graph.json |
| RC-008 (TDD sequencing) | PASS | Task 2 defines Metrics before Task 3 uses it. Task 3 adds to AppState before Task 4 uses it. |
| RC-019 (existing patterns) | PASS | Handler pattern matches existing routes.rs pattern exactly (State extractor, match arms) |
| RC-021 (file locations) | PASS | AppState at state.rs:23, metrics_stub at routes.rs:900, config at config.rs:432 -- all verified |
