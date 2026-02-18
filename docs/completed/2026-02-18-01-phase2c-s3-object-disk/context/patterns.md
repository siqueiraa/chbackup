# Pattern Discovery

## Global Patterns Registry
No `docs/patterns/` directory exists. Patterns discovered locally.

## Pattern 1: Parallel Work Queue with Semaphore

**Used in**: backup/mod.rs, upload/mod.rs, download/mod.rs, restore/mod.rs

**Structure**:
1. Flatten work items into `Vec<WorkItem>`
2. Create `Arc<Semaphore>` bounded by config concurrency
3. For each item: `tokio::spawn(async { sem.acquire(); ... })`
4. Collect all handles into Vec, await with `futures::future::try_join_all`
5. Apply results sequentially to manifest/state

**Key details**:
- Semaphore permit acquired INSIDE the spawn (not before)
- `try_join_all` provides fail-fast semantics
- Results collected as tuples: `(key, data)` for sequential application
- RateLimiter (token bucket) gates bytes AFTER the I/O operation

**Reference**: `src/upload/mod.rs:254-358`, `src/download/mod.rs:148-218`

## Pattern 2: Spawn Blocking for Sync I/O

**Used in**: backup/collect.rs, upload/stream.rs, download/stream.rs

**Structure**:
```
tokio::task::spawn_blocking(move || {
    sync_operation(&args)
})
.await
.context("task panicked")?
.context("operation failed")?
```

Two levels of `?`: first for JoinError (panic), second for the operation's Result.

**Reference**: `src/upload/mod.rs:275-280`

## Pattern 3: Manifest-Centric Data Flow

**Structure**:
- `create()` produces `BackupManifest` with parts grouped by `(table_key, disk_name)`
- `upload()` reads manifest, processes parts, updates `backup_key` and `compressed_size`
- `download()` reads manifest from S3, downloads parts, saves manifest locally
- `restore()` reads local manifest, hardlinks/attaches parts

**Key insight for S3 disk**: Parts in manifest are keyed by disk name (`HashMap<String, Vec<PartInfo>>`). An S3 disk part would have disk name like "s3disk" and would have `s3_objects: Some(Vec<S3ObjectInfo>)` set, while local parts have `s3_objects: None`.

**Reference**: `src/manifest.rs:97-141`

## Pattern 4: URL Encoding for Paths

Three variants exist:
- `backup/collect.rs::url_encode_path()` -- preserves `/`, used for filesystem paths
- `upload/mod.rs::url_encode_component()` -- does NOT preserve `/`, used for S3 key components
- `download/mod.rs::url_encode()` and `restore/attach.rs::url_encode()` -- preserves `/`

## Pattern 5: Config Concurrency Resolution

**Used in**: concurrency.rs

**Structure**: `effective_X_concurrency(config)` resolves backup-level > 0 ? backup : general

**For S3 disk**: Need `effective_object_disk_copy_concurrency(config)` following same pattern with `backup.object_disk_copy_concurrency` and `general.object_disk_server_side_copy_concurrency`.

## Pattern 6: S3Client Method Pattern

**Used in**: storage/s3.rs

**Structure**:
- All methods take `&self` (client is `Clone` via internal fields)
- Key management: `self.full_key(relative_key)` prepends prefix
- All return `anyhow::Result` with `.context()`
- Encryption/storage class applied via helper (SSE, KMS)
- S3Client struct fields: `inner` (aws_sdk_s3::Client), `bucket`, `prefix`, `storage_class`, `sse`, `sse_kms_key_id`

**Missing for S3 disk**: `copy_object(src_bucket, src_key, dst_key)` -- needs cross-bucket support

## Pattern 7: OwnedAttachParams for tokio::spawn

**Used in**: restore/attach.rs

**Structure**: When data must cross `tokio::spawn` boundary, create an `Owned*` variant with `String` instead of `&str`, `PathBuf` instead of `&Path`, `Vec<T>` instead of `&[T]`.

**Reference**: `src/restore/attach.rs:45-64`

## Pattern 8: Multi-Disk Shadow Directory Structure (CRITICAL for Phase 2c)

**Discovery**: ClickHouse FREEZE creates shadow directories PER DISK:
- Local disk: `{data_path}/shadow/{freeze_name}/store/{prefix}/{uuid}/{part}/`
- S3 disk: `{s3_disk_path}/shadow/{freeze_name}/store/{prefix}/{uuid}/{part}/`

**Current limitation**: `collect_parts()` only walks `{data_path}/shadow/` (the default local disk). For S3 disk support, must also walk each S3 disk's shadow path from `system.disks`.

**Source**: Design doc section 3.4 line 1082: "S3 disk: parse object metadata files -> collect S3 object keys"
**Source**: Design doc section 13 (clean): "Query system.disks to get all disk paths; For each disk: Remove {disk.path}/shadow/"

## Pattern 9: Disk-Aware Part Grouping

**Current code**: `backup/mod.rs:293` hardcodes `parts_by_disk.insert("default".to_string(), parts_for_table)`
**Required**: Group parts by actual disk name from the shadow walk. Each disk's shadow directory produces parts keyed by that disk name in `TableManifest.parts`.

## Anti-Patterns to Avoid

1. **Do NOT add S3 disk handling inside `spawn_blocking`** -- CopyObject is async, must stay in async context
2. **Do NOT share mutable manifest across spawns** -- collect results, apply sequentially (Pattern 1)
3. **Do NOT use a SEPARATE pipeline for S3 disk** -- integrate into existing work queue with disk type routing (Pattern 3)
4. **Do NOT hardcode disk detection** -- use `system.disks` query result to determine which disks are S3 type (DiskRow.disk_type == "s3" || "object_storage")
