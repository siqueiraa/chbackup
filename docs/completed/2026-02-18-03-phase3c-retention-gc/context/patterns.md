# Pattern Discovery

## Global Patterns
No global `docs/patterns/` directory exists. Full local pattern discovery performed.

## Discovered Patterns

### Pattern 1: List + Filter + Delete (list.rs)

Used by `clean_broken_local()` and `clean_broken_remote()` -- the closest analogues to retention.

**Local variant (sync):**
```rust
pub fn clean_broken_local(data_path: &str) -> Result<usize> {
    let backups = list_local(data_path)?;
    let broken: Vec<&BackupSummary> = backups.iter().filter(|b| b.is_broken).collect();
    // ... iterate and delete each
    let mut deleted = 0;
    for b in &broken {
        match delete_local(data_path, &b.name) {
            Ok(()) => { deleted += 1; }
            Err(e) => { warn!(...); }
        }
    }
    Ok(deleted)
}
```

**Remote variant (async):**
```rust
pub async fn clean_broken_remote(s3: &S3Client) -> Result<usize> {
    let backups = list_remote(s3).await?;
    let broken: Vec<&BackupSummary> = backups.iter().filter(|b| b.is_broken).collect();
    // ... iterate and delete each
    for b in &broken {
        match delete_remote(s3, &b.name).await {
            Ok(()) => { deleted += 1; }
            Err(e) => { warn!(...); }
        }
    }
    Ok(deleted)
}
```

**Key observations:**
- Returns `Result<usize>` (count of deleted items)
- Filters the list, then deletes individually with error handling per item
- Errors on individual deletes are warnings, not fatal
- Uses `info!` for success, `warn!` for failure per item

### Pattern 2: Server Route Handler (routes.rs)

All operation endpoints follow this exact pattern:
```rust
pub async fn handler(
    State(state): State<AppState>,
    // optional: Path(name), body: Option<Json<T>>
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let (id, _token) = state.try_start_op("op_name").await.map_err(|e| {
        (StatusCode::CONFLICT, Json(ErrorResponse { error: e.to_string() }))
    })?;
    let state_clone = state.clone();
    tokio::spawn(async move {
        let start_time = std::time::Instant::now();
        let result = /* call operation function */;
        let duration = start_time.elapsed().as_secs_f64();
        match result {
            Ok(_) => {
                if let Some(m) = &state_clone.metrics { /* record success */ }
                state_clone.finish_op(id).await;
            }
            Err(e) => {
                if let Some(m) = &state_clone.metrics { /* record failure */ }
                state_clone.fail_op(id, e.to_string()).await;
            }
        }
    });
    Ok(Json(OperationStarted { id, status: "started".into() }))
}
```

**Key observations:**
- Returns immediately with action ID
- Spawns background task
- Records duration, success/error metrics
- Uses `try_start_op` for concurrency control

### Pattern 3: CLI Command Dispatch (main.rs)

```rust
Command::CleanBroken { location } => {
    let s3 = S3Client::new(&config.s3).await?;
    let loc = map_cli_location(location);
    list::clean_broken(&config.clickhouse.data_path, &s3, &loc).await?;
    info!("CleanBroken command complete");
}
```

**Key observations:**
- Config-driven client construction
- Direct function call (no background task for CLI mode)
- Logs completion at info level

### Pattern 4: Lock Scope (lock.rs)

```rust
"clean" | "clean_broken" | "delete" => LockScope::Global,
```

Retention/clean commands use the Global lock scope, not per-backup.

### Pattern 5: BackupSummary Sort + Filter

`list_local()` and `list_remote()` already sort by name:
```rust
summaries.sort_by(|a, b| a.name.cmp(&b.name));
```

For retention, we need to sort by timestamp (oldest first) to delete the oldest. BackupSummary has `timestamp: Option<DateTime<Utc>>` and `is_broken: bool`.

### Pattern 6: S3 Key Collection for Deletion

`delete_remote()` shows the pattern for listing all keys under a backup prefix and batch-deleting:
```rust
let objects = s3.list_objects(&prefix).await?;
let keys: Vec<String> = objects.iter()
    .map(|obj| strip_s3_prefix(&obj.key, s3_prefix))
    .collect();
s3.delete_objects(keys).await?;
```

### Pattern 7: Stub-to-Implementation Upgrade (routes.rs)

The `clean_stub` endpoint currently returns 501:
```rust
pub async fn clean_stub() -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, "not implemented (Phase 3c)")
}
```

This will be replaced with a real handler following Pattern 2. The route is already wired:
```rust
.route("/api/v1/clean", post(routes::clean_stub))
```

## Patterns Summary

| Pattern | Where Used | Plan Relevance |
|---------|-----------|----------------|
| List + Filter + Delete | list.rs clean_broken_* | Template for retention_local/retention_remote |
| Server Route Handler | routes.rs all ops | Template for /api/v1/clean handler |
| CLI Command Dispatch | main.rs | Template for `clean` command dispatch |
| Lock Scope | lock.rs | Retention needs Global lock |
| BackupSummary Sort | list.rs | Sort by timestamp for retention ordering |
| S3 Key Collection | list.rs delete_remote | Template for GC key enumeration |
| Stub Replacement | routes.rs | Replace clean_stub with real handler |
