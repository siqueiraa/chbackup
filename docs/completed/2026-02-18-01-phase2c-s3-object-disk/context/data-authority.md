# Data Authority Analysis

## Data Requirements for Phase 2c

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Disk type (local vs s3) | DiskRow from system.disks | `disk_type`: "local", "s3", "object_storage" | USE EXISTING |
| Disk name | DiskRow from system.disks | `name` | USE EXISTING |
| Disk path | DiskRow from system.disks | `path` | USE EXISTING |
| Part disk assignment | system.parts or shadow metadata | Part lives under disk's shadow subdir | MUST IMPLEMENT -- shadow walk currently assumes all parts are local disk |
| Object metadata files | ClickHouse shadow directory | Files like `{part}/data.bin` that are metadata pointers | MUST IMPLEMENT -- need parser for 5 format versions |
| Object S3 paths | Object disk metadata file content | Parsed from metadata text format | MUST IMPLEMENT -- new module for metadata parsing |
| Table UUID | TableRow.uuid from system.tables | `uuid: String` | USE EXISTING |
| S3 CopyObject | aws-sdk-s3 | `copy_object` API | MUST IMPLEMENT -- S3Client has no copy_object method yet |
| Object disk copy concurrency | Config | `backup.object_disk_copy_concurrency: u32` (default 8) | USE EXISTING |
| Server-side copy concurrency | Config | `general.object_disk_server_side_copy_concurrency: u32` (default 32) | USE EXISTING |
| Object disk backup path prefix | Config | `s3.object_disk_path: String` | USE EXISTING |
| Streaming fallback allowed | Config | `s3.allow_object_disk_streaming: bool` (default false) | USE EXISTING |
| Part name and CRC64 | PartInfo | `name`, `checksum_crc64` | USE EXISTING |
| S3 object references per part | PartInfo | `s3_objects: Option<Vec<S3ObjectInfo>>` | USE EXISTING (struct exists, currently always None for local parts) |
| ListObjectsV2 for same-name optimization | S3Client | `list_objects(prefix) -> Vec<S3Object>` | USE EXISTING |

## Analysis Notes

1. **Config already prepared**: All S3 object disk config params already exist in `config.rs`:
   - `s3.object_disk_path` -- separate S3 prefix for object disk backup data
   - `s3.allow_object_disk_streaming` -- fallback flag for CopyObject failure
   - `backup.object_disk_copy_concurrency` -- semaphore for object copies
   - `general.object_disk_server_side_copy_concurrency` -- higher default for server-side ops

2. **Manifest already prepared**: `S3ObjectInfo` struct already exists in `manifest.rs` with fields for `path`, `size`, `backup_key`. The `PartInfo.s3_objects` field is `Option<Vec<S3ObjectInfo>>` -- already handles mixed local/S3 parts.

3. **Missing S3 capability**: `S3Client` has no `copy_object` method. The aws-sdk-s3 `CopyObject` API requires `copy_source` in format `bucket/key`. Must implement both same-bucket and cross-bucket copy.

4. **Shadow walk needs disk awareness**: Currently `collect_parts()` hardlinks ALL shadow files assuming local disk. For S3 disk parts, the shadow files are small metadata pointers (not data). They must be parsed to extract S3 object paths, not hardlinked for upload. The walk needs to detect which disk a part belongs to.

5. **Disk detection strategy**: Per design doc, detect S3 disks via `system.disks` where `type = 's3' OR type = 'object_storage'` (CH 24.1+). Map disk paths to shadow directory structure to determine which parts are on S3 disks.

## Each "MUST IMPLEMENT" Justification

| Item | Why Not Existing |
|------|-----------------|
| Part disk assignment detection | `collect_parts()` treats all parts as local; need shadow path -> disk mapping |
| Object metadata file parser | 5 format versions with different fields; pure text parsing, no external data |
| S3 CopyObject wrapper | S3Client only has get/put/delete/list/multipart -- no copy |
| Object disk copy concurrency helper | `concurrency.rs` has upload/download/max_connections but not object disk copy |
