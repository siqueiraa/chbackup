# Patterns

## Existing Patterns to Follow

### Config Wiring Pattern
```rust
// In config.rs: field is already parsed via serde
pub field_name: Type,

// In consuming module: read from config
let value = config.section.field_name;
```

All 35 gaps follow this pattern: the config field EXISTS and is PARSED, but the consuming module never READS it.

### S3Client Field Pattern
```rust
pub struct S3Client {
    inner: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
    storage_class: String,  // ← already stored
    sse: String,
    sse_kms_key_id: String,
}
```
New fields (acl, concurrency) follow same pattern: store in struct, apply in operations.

### Retry Pattern (existing)
```rust
// copy_object_with_retry uses fixed backoff
let delays = [100, 400, 1600]; // ms
for (attempt, delay_ms) in delays.iter().enumerate() {
    match self.copy_object(...).await {
        Ok(_) => return Ok(()),
        Err(e) if attempt < delays.len() - 1 => {
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
        }
        Err(e) => { /* fallback or error */ }
    }
}
```
Jitter should be added: `delay_ms * (1.0 + rng.f64() * jitter_factor)`

### Error Code Handling Pattern (backup)
```rust
// Existing pattern in backup/mod.rs
if let Some(db_err) = e.downcast_ref::<clickhouse::error::Error>() {
    if db_err.code() == Some(60) || db_err.code() == Some(81) {
        // table/database doesn't exist, skip
    }
}
```
Add 218 (CANNOT_FREEZE_PARTITION) to this pattern.

### StatusCode Pattern (server)
```rust
// All 12 occurrences use same pattern
return Err((
    StatusCode::CONFLICT,  // ← change to StatusCode::LOCKED
    "operation already in progress".to_string(),
));
```

### Multipart Upload Pattern (existing)
```rust
let upload_id = self.create_multipart_upload(key).await?;
let mut parts = Vec::new();
for (i, chunk) in data.chunks(chunk_size).enumerate() {
    let etag = self.upload_part(key, &upload_id, (i + 1) as i32, chunk.to_vec()).await?;
    parts.push(((i + 1) as i32, etag));
}
self.complete_multipart_upload(key, &upload_id, parts).await?;
```
Multipart CopyObject follows same pattern but uses `upload_part_copy` instead of `upload_part`.
