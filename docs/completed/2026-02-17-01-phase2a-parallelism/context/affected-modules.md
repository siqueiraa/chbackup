# Affected Modules Analysis

## Summary

- **Modules to update:** 5
- **Modules to create:** 0
- **Git base:** master (be8f01f)

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action |
|--------|------------------|----------|--------|
| src/backup | EXISTS | new_patterns (parallel FREEZE, semaphore) | UPDATE |
| src/upload | EXISTS | new_patterns (parallel upload, multipart, rate limiting) | UPDATE |
| src/download | EXISTS | new_patterns (parallel download, rate limiting) | UPDATE |
| src/restore | EXISTS | new_patterns (parallel table restore, parallel ATTACH) | UPDATE |
| src/storage | EXISTS | new_patterns (multipart upload methods) | UPDATE |

## Files Modified (Non-Module)

| File | Change |
|------|--------|
| Cargo.toml | Add `futures` crate dependency |
| src/config.rs | No structural changes (concurrency params already exist) |
| src/main.rs | No changes needed (commands already call module entry points) |

## CLAUDE.md Tasks to Generate

After implementation:
1. **Update:** src/backup/CLAUDE.md (add parallel FREEZE pattern, semaphore usage)
2. **Update:** src/upload/CLAUDE.md (add parallel upload, multipart, rate limiting)
3. **Update:** src/download/CLAUDE.md (add parallel download, rate limiting)
4. **Update:** src/restore/CLAUDE.md (add parallel table restore, conditional parallel ATTACH)
5. **Update:** src/storage/CLAUDE.md (add multipart upload API)

## Detailed Impact by Module

### src/backup/
- `mod.rs`: Refactor the sequential `for table_row in &filtered_tables` loop to use `tokio::spawn` + `Arc<Semaphore>` bounded by `max_connections`
- `freeze.rs`: Adapt FreezeGuard for per-task usage; each task gets own FreezeInfo and ensures UNFREEZE on drop/cancel
- `collect.rs`: No structural changes (already runs via spawn_blocking)

### src/upload/
- `mod.rs`: Replace sequential part iteration with flat work queue + `tokio::spawn` + `Arc<Semaphore>` bounded by `upload_concurrency`; manifest mutation collected post-join
- `stream.rs`: Add multipart upload path for parts with `size > 32MB`; add rate-limited byte stream wrapper

### src/download/
- `mod.rs`: Replace sequential part iteration with flat work queue + `tokio::spawn` + `Arc<Semaphore>` bounded by `download_concurrency`
- `stream.rs`: No structural changes (decompression stays sync via spawn_blocking)

### src/restore/
- `mod.rs`: Refactor sequential table loop to parallel with `max_connections` semaphore
- `attach.rs`: Add parallel ATTACH for plain MergeTree (when `!needs_sequential_attach(engine)`), keep sequential sorted ATTACH for Replacing/Collapsing
- `sort.rs`: No changes needed

### src/storage/
- `s3.rs`: Add `create_multipart_upload`, `upload_part`, `complete_multipart_upload`, `abort_multipart_upload` methods to S3Client
