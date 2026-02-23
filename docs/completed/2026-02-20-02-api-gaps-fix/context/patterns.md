# Pattern Discovery

No global patterns directory exists (`docs/patterns/` not found).

## Patterns Discovered from Codebase Analysis

### Pattern 1: Route Handler Delegation (routes.rs)

All operation endpoints follow this exact pattern:

```rust
async fn handler(
    State(state): State<AppState>,
    ...,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let (id, _token) = state.try_start_op("command").await.map_err(|e| {
        (StatusCode::LOCKED, Json(ErrorResponse { error: e.to_string() }))
    })?;

    let state_clone = state.clone();
    tokio::spawn(async move {
        let config = state_clone.config.load();
        let ch = state_clone.ch.load();
        // ... call actual command function ...
        let start_time = std::time::Instant::now();
        let result = crate::module::function(&config, ...).await;
        let duration = start_time.elapsed().as_secs_f64();

        match result {
            Ok(_) => {
                if let Some(m) = &state_clone.metrics {
                    m.backup_duration_seconds.with_label_values(&["command"]).observe(duration);
                    m.successful_operations_total.with_label_values(&["command"]).inc();
                }
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                if let Some(m) = &state_clone.metrics {
                    m.backup_duration_seconds.with_label_values(&["command"]).observe(duration);
                    m.errors_total.with_label_values(&["command"]).inc();
                }
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });

    Ok(Json(OperationStarted { id, status: "started".to_string() }))
}
```

**Reference implementations:**
- `create_backup()` at routes.rs:342 (simplest -- single command)
- `upload_backup()` at routes.rs:434 (with manifest cache invalidation)
- `delete_backup()` at routes.rs:956 (with path parameter extraction)

### Pattern 2: Signal Handler Registration (server/mod.rs)

Signal handlers follow this pattern:

```rust
#[cfg(unix)]
{
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sig = signal(SignalKind::xxx()).expect("failed to register SIGxxx handler");
        loop {
            sig.recv().await;
            info!("SIGxxx received, doing action");
            // ... action ...
        }
    });
}
```

**Reference implementations:**
- SIGHUP handler at mod.rs:216-228 (sends reload signal via channel)
- SIGQUIT handler at mod.rs:239-254 (captures backtrace, non-terminating loop)

### Pattern 3: BackupSummary Construction (list.rs)

Both `list_local` and `list_remote` build BackupSummary from BackupManifest:

```rust
BackupSummary {
    name: manifest.name.clone(),
    timestamp: Some(manifest.timestamp),
    size: total_uncompressed_size(&manifest),
    compressed_size: manifest.compressed_size,
    table_count: manifest.tables.len(),
    metadata_size: manifest.metadata_size,
    rbac_size: manifest.rbac_size,
    config_size: manifest.config_size,
    is_broken: false,
    broken_reason: None,
}
```

Both `parse_backup_summary()` (local, line 1187) and `list_remote()` (line 375) construct BackupSummary identically. Any new fields must be added to BOTH sites plus the broken-backup fallback variants.

### Pattern 4: Tables Pagination (routes.rs)

The tables endpoint shows the pagination pattern we need for list:

```rust
// Apply pagination (offset/limit)
let total_count = results.len();
let offset = params.offset.unwrap_or(0);
let results: Vec<T> = if let Some(limit) = params.limit {
    results.into_iter().skip(offset).take(limit).collect()
} else {
    if offset > 0 { /* log */ }
    results.into_iter().skip(offset).collect()
};

Ok((
    [(
        axum::http::header::HeaderName::from_static("x-total-count"),
        axum::http::header::HeaderValue::from_str(&total_count.to_string())
            .unwrap_or_else(|_| axum::http::header::HeaderValue::from_static("0")),
    )],
    Json(results),
))
```

Reference: `tables()` handler at routes.rs:1353-1525.
