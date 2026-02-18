# Redundancy Analysis

## Proposed New Public Components

| # | Proposed Component | Type | Module |
|---|---|---|---|
| 1 | `ObjectDiskMetadata` | pub struct | src/object_disk.rs (NEW) |
| 2 | `ObjectRef` | pub struct | src/object_disk.rs (NEW) |
| 3 | `parse_object_disk_metadata()` | pub fn | src/object_disk.rs (NEW) |
| 4 | `rewrite_metadata()` | pub fn | src/object_disk.rs (NEW) |
| 5 | `serialize_metadata()` | pub fn | src/object_disk.rs (NEW) |
| 6 | `S3Client::copy_object()` | pub async fn | src/storage/s3.rs |
| 7 | `S3Client::copy_object_streaming()` | pub async fn | src/storage/s3.rs |
| 8 | `effective_object_disk_copy_concurrency()` | pub fn | src/concurrency.rs |
| 9 | `is_s3_disk()` | pub fn | src/object_disk.rs or src/backup/collect.rs |

## Search Results

### 1. ObjectDiskMetadata (new struct)

**Search**: grep for `ObjectDisk`, `MetadataFormat`, `DiskMetadata` in src/
**Result**: No matches. No existing type for ClickHouse object disk metadata parsing.
**Decision**: **COEXIST** -- genuinely new capability, no overlap.

### 2. ObjectRef (new struct)

**Search**: grep for `ObjectRef`, `ObjRef` in src/
**Result**: `S3ObjectInfo` exists in `src/manifest.rs:144` with fields `path: String, size: u64, backup_key: String`.
**Analysis**: `S3ObjectInfo` is for manifest serialization (JSON), while `ObjectRef` is for parsed metadata (text format). `S3ObjectInfo` has `backup_key` which is set during upload. `ObjectRef` is a parse-time intermediate type.
**Decision**: **COEXIST** -- different lifecycle and purpose. ObjectRef is internal parse output; S3ObjectInfo is manifest I/O.
**Cleanup deadline**: N/A -- they serve different stages of the pipeline.

### 3-5. parse_object_disk_metadata, rewrite_metadata, serialize_metadata (new functions)

**Search**: grep for `parse_metadata`, `rewrite_meta`, `object_disk` functions in src/
**Result**: No matches.
**Decision**: **COEXIST** -- genuinely new capability.

### 6. S3Client::copy_object (new method)

**Search**: grep for `copy_object`, `CopyObject` in src/
**Result**: No matches in implementation. Only in design doc.
**Decision**: **COEXIST** -- S3 CopyObject is a fundamentally new operation.

### 7. S3Client::copy_object_streaming (new method)

**Search**: grep for `streaming_copy`, `copy_stream` in src/
**Result**: No matches.
**Decision**: **COEXIST** -- fallback path for cross-region copies.

### 8. effective_object_disk_copy_concurrency (new function)

**Search**: grep for `object_disk.*concurrency` in src/concurrency.rs
**Result**: No match. Config fields exist (`backup.object_disk_copy_concurrency`, `general.object_disk_server_side_copy_concurrency`) but no resolver function.
**Decision**: **COEXIST** -- follows existing pattern (effective_upload_concurrency, etc.)

### 9. is_s3_disk (new function)

**Search**: grep for `is_s3`, `s3_disk`, `disk_type.*s3` in src/
**Result**: No function exists. Design doc says `type = 's3' OR type = 'object_storage'`.
**Decision**: **COEXIST** -- small utility, no overlap.

## Exclusions (Not Searched)

- Trait impls (From, Debug, Serialize, Deserialize) on new types
- Builder/constructor methods (new(), parse())
- Test code

## Summary

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| ObjectDiskMetadata | (none) | COEXIST | - | New capability |
| ObjectRef | S3ObjectInfo (different lifecycle) | COEXIST | - | Parse-time vs manifest-time |
| parse_object_disk_metadata | (none) | COEXIST | - | New capability |
| rewrite_metadata | (none) | COEXIST | - | New capability |
| serialize_metadata | (none) | COEXIST | - | New capability |
| S3Client::copy_object | (none) | COEXIST | - | New S3 operation |
| S3Client::copy_object_streaming | (none) | COEXIST | - | Fallback for copy_object |
| effective_object_disk_copy_concurrency | (none) | COEXIST | - | Follows existing pattern |
| is_s3_disk | (none) | COEXIST | - | Utility function |

No REPLACE or REUSE decisions needed. All proposed components are genuinely new.
