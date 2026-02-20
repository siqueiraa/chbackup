# Pattern Discovery

No global patterns directory exists (`docs/patterns/` is empty). This project does not use Kameo actors. Patterns are discovered from existing code.

## Component Identification

### Gap 1: rbac_size/config_size
- **Manifest struct**: `BackupManifest` in `src/manifest.rs`
- **Backup RBAC**: `backup_rbac_and_configs()` in `src/backup/rbac.rs`
- **List summary**: `BackupSummary` in `src/list.rs`
- **API response**: `ListResponse` in `src/server/routes.rs`
- **Conversion**: `summary_to_list_response()` in `src/server/routes.rs`

### Gap 2: API tables pagination
- **Endpoint**: `tables()` in `src/server/routes.rs`
- **Query params**: `TablesParams` in `src/server/routes.rs`
- **Response**: `TablesResponseEntry` in `src/server/routes.rs`

### Gap 3: Remote manifest caching
- **List remote**: `list_remote()` in `src/list.rs`
- **GC keys**: `gc_collect_referenced_keys()` in `src/list.rs`
- **Retention**: `retention_remote()` in `src/list.rs`
- **Server state**: `AppState` in `src/server/state.rs`

### Gap 4: SIGQUIT stack dump
- **Server signals**: `src/server/mod.rs` (SIGINT, SIGHUP only)
- **CLI signals**: `src/main.rs` (watch mode SIGINT, SIGHUP)

### Gap 5: Streaming multipart upload
- **Upload entry**: `upload()` in `src/upload/mod.rs`
- **Compression**: `compress_part()` in `src/upload/stream.rs`
- **S3 multipart**: `create_multipart_upload`, `upload_part`, `complete_multipart_upload` in `src/storage/s3.rs`

## Reference Patterns

### Pattern 1: Adding a manifest field (from Phase 5 metadata_size)
1. Add field to `BackupManifest` with `#[serde(default)]`
2. Populate field in `backup::create()` after collecting parts
3. Read field in `list.rs` when building `BackupSummary`
4. Pass through in `summary_to_list_response()`
5. Backward compatible: `#[serde(default)]` means old manifests deserialize with 0

### Pattern 2: Signal handler (from SIGHUP in server/mod.rs)
```rust
#[cfg(unix)]
{
    let reload_tx_clone = reload_tx.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sighup = signal(SignalKind::hangup())
            .expect("failed to register SIGHUP handler");
        loop {
            sighup.recv().await;
            info!("SIGHUP received, triggering config reload");
            reload_tx_clone.send(true).ok();
        }
    });
}
```

### Pattern 3: Query parameter extension (from ListParams)
```rust
#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub location: Option<String>,
    pub desc: Option<bool>,
}
```
All query params are `Option<T>` for backward compatibility.

### Pattern 4: Buffered upload with multipart fallback (from upload/mod.rs)
1. `compress_part()` via `spawn_blocking` -> `Vec<u8>`
2. Check `should_use_multipart(compressed.len())`
3. If small: `put_object_with_retry(key, compressed, retry_config)`
4. If large: `create_multipart_upload` -> chunk loop `upload_part_with_retry` -> `complete_multipart_upload`

### Pattern 5: Directory size calculation (from walkdir in backup/collect.rs)
```rust
tokio::task::spawn_blocking(move || {
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
})
```

## Pattern: No Kameo Actors

This project uses pure async Rust with tokio. No Kameo actors, no mailboxes, no message types. Phases 0.5b checklist is N/A.
