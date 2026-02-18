# CLAUDE.md -- src/upload

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `upload` command -- compresses local backup parts with tar+LZ4 and uploads them to S3. Manifest is uploaded last so a backup is only "visible" when `metadata.json` exists (design 3.6).

## Directory Structure

```
src/upload/
  mod.rs      -- Entry point: upload() reads manifest, compresses parts, uploads to S3
  stream.rs   -- compress_part(): tar directory + LZ4 frame compress to Vec<u8>
```

## Key Patterns

### Buffered Upload (Phase 1)
Phase 1 uses in-memory buffered upload: tar the part directory to `Vec<u8>`, LZ4 compress, then single `PutObject`. This avoids streaming multipart complexity (deferred to Phase 2). Acceptable because most ClickHouse parts are <100MB compressed.

### Compression Pipeline (stream.rs)
- Uses sync `tar::Builder` + sync `lz4_flex::frame::FrameEncoder` inside `spawn_blocking`
- Flow: `tar::Builder::new(FrameEncoder::new(Vec::new()))` -> `append_dir_all` -> `finish` -> compressed bytes
- Archive entry name is the part directory name (e.g., `202401_1_50_3`)

### S3 Key Format
```
# Local disk parts (compressed archives):
{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{part_name}.tar.lz4

# S3 disk parts (CopyObject, Phase 2c):
{backup_name}/objects/{original_relative_path}             -- data objects
{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{disk_name}/{part_name}/  -- metadata files

{backup_name}/metadata.json  (uploaded LAST)
```

### URL Encoding
- `url_encode_component()` percent-encodes non-alphanumeric chars except `-`, `_`, `.`
- Does NOT preserve `/` (encodes individual path components)

### Incremental Upload (--diff-from-remote)
- When `diff_from_remote` is set, `upload()` loads the remote base manifest from S3 (`{base_name}/metadata.json`) before building the work queue
- Calls `backup::diff::diff_parts(&mut manifest, &base)` to mark matching parts as carried
- Carried parts (`.source.starts_with("carried:")`) are skipped during work queue construction -- their data already exists on S3 from the base backup
- Updated manifest (with both uploaded and carried parts) is saved locally and then uploaded to S3 last (atomicity guarantee still applies)
- `compressed_size` is only counted for actually uploaded parts, not carried parts

### Mixed Disk Upload Pipeline (Phase 2c)
- Parts are split into two queues based on disk type: local parts and S3 disk parts
- **Local parts**: existing compress+upload pipeline (tar+LZ4, PutObject/multipart)
- **S3 disk parts**: server-side CopyObject for each `S3ObjectInfo` in `part.s3_objects`, plus PutObject for metadata files
- Both queues run concurrently via `futures::future::try_join_all`
- **Separate concurrency semaphore**: S3 disk CopyObject bounded by `effective_object_disk_copy_concurrency(config)` (default 8), independent from local upload concurrency
- S3 disk source bucket/prefix parsed from `DiskRow.remote_path` via `parse_s3_uri()`
- Uses `s3.copy_object_with_retry()` for retry+backoff and conditional streaming fallback
- After CopyObject: `s3_obj.backup_key` is set to the destination key in the backup bucket
- No compression for S3 disk parts (already stored as raw S3 objects)

### CopyObject Concurrency (Phase 2c)
- `object_disk_copy_semaphore` limits concurrent CopyObject operations
- Default concurrency: 8 (conservative, since backup runs alongside FREEZE)
- Configured via `backup.object_disk_copy_concurrency`
- Independent from the local upload semaphore

### Resume State Tracking (Phase 2d)
- When `resume=true` (gated by both `--resume` CLI flag AND `config.general.use_resumable_state`):
  - Loads `UploadState` from `{backup_dir}/upload.state.json` at start
  - Validates `params_hash` matches current params (backup_name, table_pattern, diff_from_remote); stale state is discarded with a warning
  - Parts whose S3 key is in `completed_keys` are skipped during work queue construction
  - After each successful part upload: key is added to state, `save_state_graceful()` writes state (non-fatal on failure per design 16.1)
  - On successful completion: `upload.state.json` is deleted
- Uses `resume::UploadState`, `resume::load_state_file`, `resume::save_state_graceful`, `resume::delete_state_file`

### Manifest Atomicity (Phase 2d)
- Instead of directly uploading `metadata.json`, uses atomic three-step pattern:
  1. Upload to `{backup_name}/metadata.json.tmp`
  2. `s3.copy_object(bucket, tmp_key, final_key)` -- atomic visibility
  3. `s3.delete_object(tmp_key)` -- cleanup
- If crash occurs between steps 1 and 2: backup has `.tmp` file but no `metadata.json` -> marked as broken by `list` command
- If crash occurs between steps 2 and 3: `.tmp` file is orphaned but harmless, cleaned by `clean_broken`
- Logs `"Manifest uploaded atomically"` on success

### Public API
- `upload(config, s3, backup_name, backup_dir, delete_local, diff_from_remote: Option<&str>, resume: bool) -> Result<()>` -- Main entry point with resume and atomic manifest (Phase 2d)
- `compress_part(part_dir, archive_name) -> Result<Vec<u8>>` -- Sync tar+LZ4 compression

### Parallel Upload Pattern (Phase 2a)
- All parts across all tables are flattened into a single `Vec<UploadWorkItem>` work queue
- Upload concurrency bounded by `effective_upload_concurrency(config)` via a `tokio::Semaphore`
- Each `tokio::spawn` task: acquires permit -> `spawn_blocking` compress -> decide single vs multipart -> upload -> `rate_limiter.consume()`
- **Multipart threshold**: compressed data > 32 MiB (`MULTIPART_THRESHOLD`) uses multipart upload; otherwise single `PutObject`
- Multipart flow: `create_multipart_upload` -> chunked `upload_part` (chunk size from `calculate_chunk_size`) -> `complete_multipart_upload`; on error, `abort_multipart_upload` for cleanup
- `RateLimiter` gates total bytes uploaded per second (0 = unlimited)
- Uses `futures::future::try_join_all` for fail-fast error propagation
- After all tasks join: results `(table_key, disk_name, PartInfo, compressed_size)` are applied to the manifest sequentially (no concurrent HashMap mutation)

### Error Handling
- Uses `anyhow::Result` with `.context()` for error chain
- Updates manifest `compressed_size` after all uploads complete
- If `delete_local` is true, removes local backup directory after successful upload

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real S3
