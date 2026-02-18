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

### URL Encoding
- `url_encode()` preserves `/` (unlike upload's `url_encode_component`) since it handles full paths

### Public API
- `download(config, s3, backup_name) -> Result<PathBuf>` -- Main entry point, returns backup dir path
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
