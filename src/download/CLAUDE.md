# CLAUDE.md -- src/download

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `download` command -- fetches a backup from S3 and decompresses parts to the local filesystem. Mirrors the upload module in reverse.

## Directory Structure

```
src/download/
  mod.rs      -- Entry point: download() fetches manifest then parts from S3
  stream.rs   -- decompress_part(): LZ4 decompress + untar to directory; also has compress/decompress helpers
```

## Key Patterns

### Buffered Download (Phase 1)
Phase 1 downloads full objects to memory via `s3.get_object()`, then decompresses. Acceptable for MVP. Phase 2 will add streaming download for large parts.

### Decompression Pipeline (stream.rs)
- Supports 4 compression formats: `lz4`, `zstd`, `gzip`, `none` (Phase 4f)
- Format driven by `manifest.data_format` -- the download pipeline reads the format from the remote manifest and passes it to `decompress_part()` (Phase 4f)
- Uses sync format-specific decompressor + sync `tar::Archive` inside `spawn_blocking`
- Format-specific behavior:
  - `lz4`: `lz4_flex::frame::FrameDecoder::new(data)` -> `Archive::new(decoder)` -> `unpack(output_dir)`
  - `zstd`: `zstd::Decoder::new(data)` -> `Archive::new(decoder)` -> `unpack(output_dir)`
  - `gzip`: `flate2::read::GzDecoder::new(data)` -> `Archive::new(decoder)` -> `unpack(output_dir)`
  - `none`: `std::io::Cursor::new(data)` -> `Archive::new(cursor)` -> `unpack(output_dir)` (just untar)
- Also exports `compress_part(part_dir, archive_name, data_format, compression_level)` (for testing) and `decompress_lz4()` (standalone utility)

### Download Flow
1. Download manifest: `s3.get_object("{backup_name}/metadata.json")` -> parse `BackupManifest`
2. Create local directory: `{data_path}/backup/{backup_name}/`
3. For each table, for each part:
   - **Local disk parts**: download compressed archive, decompress to local
   - **S3 disk parts** (Phase 2c): download only metadata files (data objects stay in backup bucket until restore)
4. Download RBAC and config directories (Phase 4e):
   - If `manifest.rbac.is_some()`: download `{backup_name}/access/*` to `{backup_dir}/access/`
   - Always attempt `{backup_name}/configs/*` download (no-op if no configs in S3)
5. Save manifest and per-table metadata locally
6. Return backup directory path

### S3 Disk Metadata-Only Download (Phase 2c)
- S3 disk parts are detected via `object_disk::is_s3_disk(disk_type)` combined with `part.s3_objects.is_some()`
- For S3 disk parts: `is_s3_disk_part` flag is set on `DownloadWorkItem`
- Flagged parts skip the full compressed archive download; instead only the metadata files describing S3 object locations are downloaded
- The actual S3 data objects are NOT downloaded -- they remain in the backup bucket and are copied directly to the data bucket during restore via CopyObject
- This optimization avoids unnecessary data transfer for S3 disk parts (data never leaves S3)

### Resume State Tracking (Phase 2d)
- When `resume=true` (gated by both `--resume` CLI flag AND `config.general.use_resumable_state`):
  - Loads `DownloadState` from `{backup_dir}/download.state.json` at start
  - Validates `params_hash` matches current params; stale state is discarded with a warning
  - Parts whose S3 key is in `completed_keys` are skipped during work queue construction
  - After each successful download+decompress: key is added to state, `save_state_graceful()` writes state (non-fatal on failure per design 16.1)
  - On successful completion: `download.state.json` is deleted
- Uses `resume::DownloadState`, `resume::load_state_file`, `resume::save_state_graceful`, `resume::delete_state_file`

### Post-Download CRC64 Verification (Phase 2d)
- After decompressing each local disk part, verifies CRC64 checksum:
  - Finds `checksums.txt` in the decompressed part directory
  - Calls `backup::checksum::compute_crc64(checksums_path)` to compute the CRC64
  - Compares against `part.checksum_crc64` from the manifest
  - On mismatch: deletes the corrupted part directory, logs error, retries download (up to `config.general.retries_on_failure` times)
  - On persistent mismatch after retries: propagates error
- S3 disk parts skip CRC64 verification (no local data to verify)

### Disk Space Pre-Flight Check (Phase 2d)
- After downloading the manifest but before the data phase:
  - Uses `nix::sys::statvfs::statvfs()` on the backup directory's parent path to check available disk space
  - Computes required space from manifest's total compressed sizes
  - Compares against `available_space * 0.95` (5% safety margin)
  - On insufficient space: returns an error with details (required vs available)
  - On `statvfs` failure (e.g., NFS): logs warning and continues (best-effort)

### Hardlink Dedup (Phase 5)
- `--hardlink-exists-files` flag enables post-download deduplication via hardlinks to existing local backups
- Before downloading each local disk part, `find_existing_part()` scans `{data_path}/backup/*/shadow/{table_key}/{part_name}/` (excluding the current backup)
- For each candidate: computes CRC64 of `checksums.txt`, compares against the manifest's expected CRC64
- If a match is found: `hardlink_existing_part()` creates hardlinks from the existing part to the target directory (with EXDEV copy fallback), skipping the S3 download entirely
- Parts with `checksum_crc64 == 0` are skipped (no valid CRC to compare)
- The check runs inside the parallel task via `spawn_blocking`, before the S3 `get_object` call
- Performance note: scans all local backups for each part (O(backups * parts)), acceptable because directory listing is fast and typically few backups exist locally

### Progress Bar Integration (Phase 5)
- `ProgressTracker` from `progress.rs` is created before the parallel download loop
- Disabled when `config.general.disable_progress_bar` is true or when not running in a TTY
- `Clone`d into each spawned download task; `tracker.inc()` called after each successful part download
- `tracker.finish()` called after all tasks join
- Shows: operation label, progress bar, percentage, part count, throughput, ETA

### URL Encoding
- `url_encode()` preserves `/` (unlike upload's `url_encode_component`) since it handles full paths

### Simple Directory Download (Phase 4e)
- `download_simple_directory(s3, backup_name, local_dir, prefix)` -- Downloads all files under `{backup_name}/{prefix}/` from S3 to `{local_dir}/{prefix}/`. Uses `s3.list_objects()` to enumerate files, then `s3.get_object()` for each. Creates local directory structure as needed. No-op if no objects exist under the prefix. Called after part downloads complete for `access/` and `configs/` directories.

### Public API
- `download(config, s3, backup_name, resume: bool, hardlink_exists_files: bool) -> Result<PathBuf>` -- Main entry point with resume support (Phase 2d) and hardlink dedup (Phase 5)
- `decompress_part(data, output_dir, data_format) -> Result<()>` -- Sync multi-format decompression (lz4, zstd, gzip, none) (Phase 4f)
- `compress_part(part_dir, archive_name, data_format, compression_level) -> Result<Vec<u8>>` -- Sync multi-format compression (for testing) (Phase 4f)
- `decompress_lz4(data) -> Result<Vec<u8>>` -- Raw LZ4 frame decompression

### Parallel Download Pattern (Phase 2a)
- All parts across all tables are flattened into a single `Vec<DownloadWorkItem>` work queue
- Download concurrency bounded by `effective_download_concurrency(config)` via a `tokio::Semaphore`
- Each `tokio::spawn` task: acquires permit -> `s3.get_object` -> `rate_limiter.consume()` -> `spawn_blocking` decompress -> returns `(table_key, compressed_size)`
- `RateLimiter` gates total bytes downloaded per second (0 = unlimited)
- Uses `futures::future::try_join_all` for fail-fast error propagation
- After all tasks join: tally totals, then save per-table metadata and manifest sequentially

### Error Handling
- Uses `anyhow::Result` with `.context()` for error chain
- Logs warnings for parts that fail to download but does not abort the entire backup
- Creates directory structure before unpacking

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real S3
