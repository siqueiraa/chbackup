# CLAUDE.md -- src/upload

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `upload` command -- compresses local backup parts with tar+LZ4 and uploads them to S3. Manifest is uploaded last so a backup is only "visible" when `metadata.json` exists (design 3.6).

## Directory Structure

```
src/upload/
  mod.rs      -- Entry point: upload() reads manifest, compresses parts, uploads to S3
  stream.rs   -- compress_part() (buffered) + compress_part_streaming() (chunked channel) for tar+compress
```

## Key Patterns

### Buffered Upload (Phase 1)
Phase 1 uses in-memory buffered upload: tar the part directory to `Vec<u8>`, LZ4 compress, then single `PutObject`. This avoids streaming multipart complexity (deferred to Phase 2). Acceptable because most ClickHouse parts are <100MB compressed.

### Compression Pipeline (stream.rs)
- Supports 4 compression formats: `lz4`, `zstd`, `gzip`, `none` (Phase 4f)
- Format selected by `config.backup.compression` (`data_format` parameter), level by `config.backup.compression_level`
- Uses sync `tar::Builder` + format-specific compressor inside `spawn_blocking`
- Flow: `tar::Builder::new(compressor(Vec::new()))` -> `append_dir_all` -> `finish` -> compressed bytes
- Archive entry name is the part directory name (e.g., `202401_1_50_3`)
- Format-specific behavior:
  - `lz4`: `lz4_flex::frame::FrameEncoder` (ignores compression level)
  - `zstd`: `zstd::Encoder::new(Vec::new(), level as i32)` with `auto_finish()`
  - `gzip`: `flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level))`
  - `none`: tar directly into `Vec<u8>` (no compression)
- `archive_extension(data_format)` maps format to file extension: `.tar.lz4`, `.tar.zstd`, `.tar.gz`, `.tar`

### S3 Key Format
```
# Local disk parts (compressed archives, extension varies by data_format):
{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{part_name}{archive_extension(data_format)}

# S3 disk parts (CopyObject, Phase 2c):
{backup_name}/objects/{original_relative_path}             -- data objects
{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{disk_name}/{part_name}/  -- metadata files

# RBAC files (Phase 4e, uncompressed):
{backup_name}/access/{filename}.jsonl                      -- RBAC entity JSONL files

# Config files (Phase 4e, uncompressed):
{backup_name}/configs/{relative_path}                      -- ClickHouse config files

{backup_name}/metadata.json  (uploaded LAST)
```

### Per-Disk Part Lookup (mod.rs)
- `find_part_dir()` delegates to `backup::collect::resolve_shadow_part_path()` for per-disk + legacy fallback path resolution
- Accepts `manifest_disks: &HashMap<String, String>`, `backup_name: &str`, and `disk_name: &str` in addition to existing parameters
- Callers in `upload()` pass `&manifest.disks`, the backup name, and each part's disk name (all already in scope)
- Error on `None` return (part not found at any location) includes details about which paths were checked

### Per-Disk Delete Local Cleanup (mod.rs)
- When `delete_local` is true after upload, per-disk backup directories are cleaned BEFORE the default backup_dir
- Iterates `manifest.disks` to discover per-disk dirs via `per_disk_backup_dir()`
- Uses `std::fs::canonicalize()` + `HashSet` dedup to prevent double-delete (e.g., when symlinks resolve to the same path)
- Per-disk dir deletion is non-fatal (warn on failure); default backup_dir deletion remains fatal (preserves existing `?` propagation semantics)

### Path Encoding
- `url_encode_component()` has been removed; all call sites now use `crate::path_encoding::encode_path_component()` which provides identical behavior (percent-encodes non-safe chars, does NOT preserve `/`)

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
- S3 disk source bucket/prefix parsed from `DiskRow.remote_path` via `parse_s3_uri()`; the CopyObject source key is rebuilt from the manifest's disk-relative key with `object_disk::upload_source_key(path, source_prefix)`, which is the exact inverse of the `disk_relative_key()` normalisation `create` applied
- Uses `s3.copy_object_with_retry()` for retry+backoff and conditional streaming fallback
- After CopyObject: `s3_obj.backup_key` is set to the destination key in the backup bucket
- No compression for S3 disk parts (already stored as raw S3 objects)

### Deferred-Freeze Finalisation
- `upload()` is a thin wrapper; the real body is `upload_inner()`. The wrapper exists to release any FREEZE that `backup::create` deliberately held so S3 object-disk objects stayed pinned until CopyObject read them (see `backup::deferred`).
- **Finalisation runs inside a spawned task, not in the wrapper.** `auto_resume` selects over the upload future and can drop it; dropping the `JoinHandle` merely *detaches* the task, so anything after the `.await` in the wrapper would never run. Only in-task cleanup survives that.
- **Nested spawn**: `upload_inner` is itself spawned so a panic becomes a `JoinError` value rather than unwinding past the finaliser.
- SIGKILL is not survivable in-process — covered by stale-record adoption in `deferred.rs`, which is why the persisted record is the primary safety mechanism and this path is the fast one.
- `finalize_deferred_freeze()` deletes the record only after a genuine unfreeze. On partial failure it rewrites the record with just the still-frozen entries (via `unfreeze_all_checked`), keeping the leak discoverable. A corrupt record leaves the FREEZE in place rather than guessing what to release.
- Because the public signature takes borrows, the wrapper clones `Config`/`ChClient`/`S3Client` (all `Clone`) to satisfy `spawn`'s `'static` bound.

### Missing Source Objects
- A vanished object **fails the upload loudly**; it is never skipped. Skipping would publish a manifest claiming completeness while the table's data is absent. Any future skip mode must also refuse to publish a complete-looking manifest.
- `storage::s3::is_missing_source_error()` classifies `NoSuchKey`/`NoSuchBucket` as permanent, so `copy_object_with_retry_jitter` returns immediately instead of burning ~2.1 s of backoff, and skips the streaming fallback (which would GET the same missing key).
- The error names db/table/part/disk and the likely causes: the part was merged away while its FREEZE was not held, or something deleted objects from the object-disk bucket directly.

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

### Progress Bar Integration (Phase 5)
- `ProgressTracker` from `progress.rs` is created before the parallel upload loop
- Disabled when `config.general.disable_progress_bar` is true or when not running in a TTY
- `Clone`d into each spawned upload task (both local and S3 disk CopyObject tasks); `tracker.inc()` called after each successful part upload
- `tracker.finish()` called after all tasks from both queues join
- Shows: operation label, progress bar, percentage, part count, throughput, ETA

### Simple Directory Upload (Phase 4e)
- `upload_simple_directory(s3, backup_name, local_dir, prefix)` -- Uploads all files from a local directory to S3 under `{backup_name}/{prefix}/`. Uses `spawn_blocking` + `walkdir` for directory traversal, then sequential `put_object` for each file. No compression (RBAC/config files are small text files). Called after part upload completes but before atomic manifest upload for `access/` and `configs/` directories.

### Streaming Multipart Upload (Phase 8)
For parts exceeding `config.backup.streaming_upload_threshold` (default 256 MiB uncompressed), the upload pipeline uses a streaming path instead of buffering the entire compressed part in memory.

- **Threshold check**: Before calling `compress_part()`, the pipeline compares `part.size` against `streaming_upload_threshold`. Parts below the threshold use the existing buffered path unchanged.
- **`compress_part_streaming()`** (stream.rs): Spawns a background `std::thread` that tars+compresses the part directory and sends fixed-size `Vec<u8>` chunks (at least `MIN_MULTIPART_CHUNK` = 5 MiB for S3 multipart compatibility) through a **bounded** `std::sync::mpsc::sync_channel`. A `ChunkedWriter` adapter buffers bytes and flushes chunks when the buffer reaches the target size. The final partial chunk is sent on writer drop.
- **Streaming upload flow**: Receives chunks from the channel, creates an S3 multipart upload, uploads each chunk as a part via `upload_part_with_retry()`, then completes the multipart. On error, `abort_multipart_upload` is called for cleanup.
- **Bounded memory (`MAX_IN_FLIGHT_CHUNKS`)**: The pipeline has *two* channels -- the compression thread's `sync_channel` (`STREAM_CHANNEL_BOUND`) and the tokio bridge to the async upload loop (`BRIDGE_CHANNEL_BOUND`) -- so more than one chunk is live at a time. `MAX_IN_FLIGHT_CHUNKS` is the total across both: one buffer per queued slot, plus one held by each of the compression thread (blocked in `send`), the bridge thread (between `recv` and `blocking_send`), and the upload loop (being sent to S3). Peak memory for a streamed part is `MAX_IN_FLIGHT_CHUNKS * chunk_size` -- 30 MiB at the 5 MiB minimum chunk size -- no matter how large the part is. **The constant is derived from both channel bounds**, so neither can be raised without the documented ceiling moving with it; a test asserts the observed in-flight peak stays within the budget. Do not restate the ceiling as a literal number anywhere.
- **Coexistence**: Both `compress_part()` (buffered, returns `Vec<u8>`) and `compress_part_streaming()` (streaming, returns `mpsc::Receiver`) coexist. The buffered path remains the default for most parts. The streaming path is only used for large parts to avoid excessive memory consumption.
- **Config**: `backup.streaming_upload_threshold` (u64, default 268435456 / 256 MiB). Set to 0 to force all parts through the streaming path; set very high to disable.

### Public API
- `upload(config, ch, s3, backup_name, backup_dir, delete_local, diff_from_remote: Option<&str>, resume: bool) -> Result<()>` -- Main entry point with resume and atomic manifest (Phase 2d)
- `compress_part(part_dir, archive_name, data_format, compression_level) -> Result<Vec<u8>>` -- Sync multi-format buffered compression (lz4, zstd, gzip, none) (Phase 4f)
- `compress_part_streaming(part_dir, archive_name, data_format, compression_level, chunk_size) -> Result<mpsc::Receiver<Result<Vec<u8>>>>` -- Sync multi-format streaming compression producing fixed-size chunks via channel (Phase 8)
- `archive_extension(data_format) -> &str` -- Maps format name to file extension (e.g., "lz4" -> ".tar.lz4") (Phase 4f)

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
