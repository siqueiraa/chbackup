# Pattern Discovery

## Global Patterns

No global `docs/patterns/` directory exists. Full local pattern discovery performed.

## Pattern 1: Sequential Command Pipeline (Phase 1 Pattern)

**Source**: `src/backup/mod.rs`, `src/upload/mod.rs`, `src/download/mod.rs`, `src/restore/mod.rs`

All Phase 1 commands follow the same sequential pattern:
1. Load manifest (or create it)
2. Iterate tables sequentially with `for table in &tables {}`
3. For each table, iterate parts sequentially with `for part in parts {}`
4. Use `tokio::task::spawn_blocking` for sync I/O (tar, lz4, walkdir)
5. Return `Result<()>` or `Result<BackupManifest>`

**Key observation**: No `tokio::spawn`, no `Semaphore`, no `try_join_all` anywhere in the codebase. The `futures` crate is not yet a dependency. Phase 2a is a greenfield introduction of parallelism.

## Pattern 2: spawn_blocking for Sync I/O

**Source**: `src/backup/mod.rs:186`, `src/upload/mod.rs:156`, `src/download/mod.rs:142`

All sync filesystem operations (walkdir, tar, lz4) use `tokio::task::spawn_blocking`:
```rust
let result = tokio::task::spawn_blocking(move || {
    sync_function(args)
})
.await
.context("spawn_blocking panicked")??;
```

Pattern: clone owned data into closure, double-? for JoinError + inner Result.

## Pattern 3: Config-Driven Concurrency Parameters

**Source**: `src/config.rs`

Concurrency params exist in TWO config sections:
- `general.upload_concurrency` (u32, default 4) -- general section
- `general.download_concurrency` (u32, default 4) -- general section
- `backup.upload_concurrency` (u32, default 4) -- backup section
- `backup.download_concurrency` (u32, default 4) -- backup section
- `clickhouse.max_connections` (u32, default 1) -- for FREEZE/restore
- `backup.object_disk_copy_concurrency` (u32, default 8)
- `general.upload_max_bytes_per_second` (u64, default 0 = unlimited)
- `general.download_max_bytes_per_second` (u64, default 0 = unlimited)
- `backup.upload_max_bytes_per_second` (u64, default 0 = unlimited)
- `backup.download_max_bytes_per_second` (u64, default 0 = unlimited)
- `s3.max_parts_count` (u32, default 10000) -- multipart parts limit
- `s3.chunk_size` (u64, default 0 = auto) -- multipart chunk size

**IMPORTANT**: There are TWO sets of concurrency/rate-limit params (general.* and backup.*). The design doc (Section 11.1) references `upload_concurrency` and `download_concurrency` as the semaphore bounds. Phase 2a should use `backup.upload_concurrency` and `backup.download_concurrency` for the flat semaphore model (matching the Go tool's `backup:` section behavior).

## Pattern 4: S3Client Wrapper (storage/s3.rs)

**Source**: `src/storage/s3.rs`

The S3Client wraps `aws_sdk_s3::Client` with:
- `full_key(relative_key)` for prefix management
- Storage class + SSE applied to all uploads
- `put_object(key, body: Vec<u8>)` -- single PutObject
- `get_object(key) -> Vec<u8>` -- full object download
- `get_object_stream(key) -> ByteStream` -- streaming download (exists but unused in Phase 1)

**Multipart upload**: Not yet implemented. The S3Config has `max_parts_count` and `chunk_size` fields but no multipart methods on S3Client.

## Pattern 5: FreezeGuard Lifecycle (backup/freeze.rs)

**Source**: `src/backup/freeze.rs`

FreezeGuard is NOT Drop-based for cleanup (async UNFREEZE cannot run in sync Drop):
- `FreezeGuard::new()` creates empty guard
- `guard.add(FreezeInfo { db, table, freeze_name })` tracks frozen tables
- `guard.unfreeze_all(ch).await?` explicitly unfreezes
- Drop impl only warns if tables remain frozen

**Phase 2a impact**: In parallel FREEZE, each spawned task needs its own FreezeInfo, and UNFREEZE must run even on task cancellation. Design says "scopeguard UNFREEZE on every task" -- this means each task should track its own freeze and ensure cleanup.

## Pattern 6: Error Handling (anyhow)

**Source**: All modules

Every public function returns `anyhow::Result<T>` with `.context()` annotations. The project does NOT use `ChBackupError` enum extensively -- it exists in `src/error.rs` but most code uses anyhow directly.

## Pattern 7: Manifest Mutation During Upload

**Source**: `src/upload/mod.rs:89-198`

The upload function loads the manifest, mutates it (setting backup_key on each part), then re-saves:
```rust
let mut manifest = BackupManifest::load_from_file(...)?;
// ... for each part: updated_part.backup_key = s3_key;
manifest.compressed_size = total_compressed_size;
manifest.save_to_file(...)?;
```

**Phase 2a impact**: With parallel uploads, parts across tasks need to update the manifest concurrently. Options: (1) collect results and apply after all tasks complete, or (2) use interior mutability. Design recommends collecting results.
