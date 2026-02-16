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
{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{part_name}.tar.lz4
{backup_name}/metadata.json  (uploaded LAST)
```

### URL Encoding
- `url_encode_component()` percent-encodes non-alphanumeric chars except `-`, `_`, `.`
- Does NOT preserve `/` (encodes individual path components)

### Public API
- `upload(config, s3, backup_name, backup_dir, delete_local) -> Result<()>` -- Main entry point
- `compress_part(part_dir, archive_name) -> Result<Vec<u8>>` -- Sync tar+LZ4 compression

### Error Handling
- Uses `anyhow::Result` with `.context()` for error chain
- Updates manifest `compressed_size` after all uploads complete
- If `delete_local` is true, removes local backup directory after successful upload

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real S3
