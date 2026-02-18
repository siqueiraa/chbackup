# Pattern Discovery

## Global Patterns Registry

No `docs/patterns/` directory exists in this project. Pattern discovery done locally.

## Component Identification

### Components for Phase 3b

| Component | Type | Location | Status |
|-----------|------|----------|--------|
| MetricsRegistry | New struct (wrapper) | `src/server/metrics.rs` (new file) | TO CREATE |
| Metrics endpoint handler | Route handler | `src/server/routes.rs` | REPLACE stub |
| AppState | Shared state | `src/server/state.rs` | EXTEND (add metrics field) |
| Operation handlers | Route handlers | `src/server/routes.rs` | MODIFY (instrument with metrics) |
| prometheus crate | Dependency | `Cargo.toml` | TO ADD |

### Pattern: Route Handler (from routes.rs)

Every operation endpoint follows:
```rust
async fn handler(State(state): State<AppState>, ...) -> Result<...> {
    let (id, _token) = state.try_start_op("command").await.map_err(|e| ...)?;
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

### Pattern: AppState Extension

AppState fields are `Arc`-wrapped or `Clone`. New fields follow existing pattern:
```rust
pub struct AppState {
    pub config: Arc<Config>,
    pub ch: ChClient,
    pub s3: S3Client,
    pub action_log: Arc<Mutex<ActionLog>>,
    pub current_op: Arc<Mutex<Option<RunningOp>>>,
    pub op_semaphore: Arc<Semaphore>,
    // NEW: pub metrics: Option<Arc<Metrics>>,  -- None when enable_metrics=false
}
```

### Pattern: Prometheus Crate Usage

Standard prometheus crate pattern (v0.13):
```rust
use prometheus::{Registry, TextEncoder, Encoder, IntCounter, IntGauge, Histogram, HistogramOpts, opts};

// Create registry
let registry = Registry::new();

// Register metrics
let counter = IntCounter::new("name", "help")?;
registry.register(Box::new(counter.clone()))?;

// Expose via HTTP
let encoder = TextEncoder::new();
let mut buffer = Vec::new();
encoder.encode(&registry.gather(), &mut buffer)?;
String::from_utf8(buffer)?
```

### Pattern: Metrics Endpoint (axum + prometheus)

```rust
async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    // Update gauge-type metrics that need refreshing
    // Encode and return text/plain
}
```

### Pattern: Operation Instrumentation

Metrics recording happens in the spawned task's match arms:
```rust
tokio::spawn(async move {
    let start = std::time::Instant::now();
    let result = do_operation(...).await;
    let duration = start.elapsed().as_secs_f64();
    match result {
        Ok(_) => {
            if let Some(m) = &state_clone.metrics {
                m.duration.with_label_values(&["create"]).observe(duration);
                m.last_success_timestamp.set(chrono::Utc::now().timestamp() as f64);
            }
            state_clone.finish_op(id).await;
        }
        Err(e) => {
            if let Some(m) = &state_clone.metrics {
                m.errors_total.with_label_values(&["create"]).inc();
            }
            state_clone.fail_op(id, e.to_string()).await;
        }
    }
});
```

## Design Decisions

1. **`Option<Arc<Metrics>>`** -- When `enable_metrics` is false, no prometheus registry is created. Metrics recording is gated by `if let Some(m) = &state.metrics`.

2. **Custom Registry (not global)** -- Use `prometheus::Registry::new()` rather than the global default registry. This allows clean testing and avoids interference between metrics in test cases.

3. **Label-based counters/histograms** -- Use `with_label_values(&["operation"])` for per-operation breakdown rather than separate metrics per operation. This is the standard Prometheus pattern.

4. **Gauge refresh on scrape** -- Backup counts (`number_backups_local`, `number_backups_remote`) are expensive to compute (directory scan, S3 list). Two options:
   - Option A: Refresh on every `/metrics` scrape (simple but potentially slow)
   - Option B: Cache with TTL (complex)
   - **Decision: Option A** -- backup count operations are fast enough for typical 15-30s scrape intervals. Local is a directory listing; remote is an S3 ListObjects which is already cached by the SDK.
