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
- Uses sync `lz4_flex::frame::FrameDecoder` + sync `tar::Archive` inside `spawn_blocking`
- Flow: `FrameDecoder::new(data)` -> `Archive::new(decoder)` -> `unpack(output_dir)`
- Also exports `compress_part()` and `decompress_lz4()` utilities for testing

### Download Flow
1. Download manifest: `s3.get_object("{backup_name}/metadata.json")` -> parse `BackupManifest`
2. Create local directory: `{data_path}/backup/{backup_name}/`
3. For each table, for each part:
   - **Local disk parts**: download compressed archive, decompress to local
   - **S3 disk parts** (Phase 2c): download only metadata files (data objects stay in backup bucket until restore)
4. Save manifest and per-table metadata locally
5. Return backup directory path

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

### URL Encoding
- `url_encode()` preserves `/` (unlike upload's `url_encode_component`) since it handles full paths

### Public API
- `download(config, s3, backup_name, resume: bool) -> Result<PathBuf>` -- Main entry point with resume support (Phase 2d)
- `decompress_part(data, output_dir) -> Result<()>` -- Sync LZ4+untar decompression
- `compress_part(part_dir, archive_name) -> Result<Vec<u8>>` -- Sync tar+LZ4 (for testing)
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
