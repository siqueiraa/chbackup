# Affected Modules Analysis

## Summary

- **Modules to update:** 5
- **New files to create:** 1 (src/object_disk.rs)
- **Root files modified:** 2 (src/lib.rs, src/concurrency.rs)
- **Git base:** b241320

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action |
|--------|------------------|----------|--------|
| src/backup | EXISTS | new_patterns | UPDATE |
| src/upload | EXISTS | new_patterns | UPDATE |
| src/download | EXISTS | new_patterns | UPDATE |
| src/restore | EXISTS | new_patterns | UPDATE |
| src/storage | EXISTS | new_patterns | UPDATE |

## New Files

| File | Purpose |
|------|---------|
| src/object_disk.rs | ClickHouse object disk metadata parsing (5 format versions), metadata rewriting for restore |

## Module-Level Changes

### src/backup
- `collect.rs`: Disk-aware shadow walk -- detect S3 disk parts by checking metadata files, parse object references instead of hardlinking data
- `mod.rs`: Build `parts_by_disk` HashMap with actual disk names (not just "default"); populate `s3_objects` on PartInfo for S3 disk parts

### src/upload
- `mod.rs`: Two work item types -- local disk parts go through compress+upload pipeline, S3 disk parts go through CopyObject pipeline with separate semaphore

### src/download
- `mod.rs`: For S3 disk parts, download only metadata (not data objects); save metadata files locally for restore to rewrite

### src/restore
- `attach.rs`: S3 disk restore path -- CopyObject to UUID-isolated paths, rewrite metadata files, write to detached/

### src/storage
- `s3.rs`: New `copy_object()` and `copy_object_streaming()` methods

## CLAUDE.md Tasks to Generate

1. **Update:** src/backup/CLAUDE.md (new disk-aware patterns)
2. **Update:** src/upload/CLAUDE.md (mixed disk upload pipeline)
3. **Update:** src/download/CLAUDE.md (S3 disk part handling)
4. **Update:** src/restore/CLAUDE.md (S3 disk restore with UUID isolation)
5. **Update:** src/storage/CLAUDE.md (copy_object methods)
