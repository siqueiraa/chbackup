# Plan: Phase 2c -- S3 Object Disk Support

## Goal

Add ClickHouse S3 object disk support to all four command pipelines (create, upload, download, restore). Parts on S3 disks are backed up via server-side CopyObject instead of compress+upload, and restored with UUID-isolated object paths and rewritten metadata files.

## Architecture Overview

ClickHouse can store data parts on S3 "object disks" in addition to local disks. A single table may have parts on both local and S3 disks (mixed/tiered storage). This plan adds:

1. **Object disk metadata parser** (`src/object_disk.rs`) -- Parses all 5 ClickHouse metadata format versions to extract S3 object paths from frozen shadow files.
2. **S3 CopyObject** (`src/storage/s3.rs`) -- Server-side copy between buckets (or within a bucket) with retry+backoff and conditional streaming fallback gated by `allow_object_disk_streaming`.
3. **Disk-aware backup** (`src/backup/`) -- Shadow walk detects S3 disk parts via metadata files, parses object references instead of hardlinking data.
4. **Mixed disk upload** (`src/upload/`) -- Local parts go through compress+upload; S3 disk parts use CopyObject with a separate concurrency semaphore.
5. **S3 disk download** (`src/download/`) -- S3 disk parts download only metadata files (data objects stay in backup bucket until restore).
6. **UUID-isolated restore** (`src/restore/`) -- Copies S3 objects to paths derived from the destination table's UUID, rewrites metadata, writes to detached/.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **S3Client**: Owned by caller, passed by clone into spawned tasks. `copy_object` and `copy_object_streaming` are new methods added to the existing S3Client.
- **ObjectDiskMetadata**: Parse-time struct. Created by `parse_metadata()`, consumed by backup (to build `S3ObjectInfo` list) and restore (to rewrite paths).
- **Concurrency**: Two separate helpers: `effective_object_disk_copy_concurrency()` resolves `backup.object_disk_copy_concurrency` (for backup/upload CopyObject), and `effective_object_disk_server_side_copy_concurrency()` resolves `general.object_disk_server_side_copy_concurrency` (for restore CopyObject). These are independent semaphores, not a fallback chain.
- **Manifest**: `PartInfo.s3_objects` field already exists (`Option<Vec<S3ObjectInfo>>`), currently always `None`. Phase 2c populates it for S3 disk parts.
- **disk_type_map**: Already built from `system.disks` query in `backup/mod.rs:87-90` and stored in `manifest.disk_types`. Phase 2c uses it for routing.

### What This Plan CANNOT Do
- Cannot test cross-region CopyObject fallback without two separate S3 regions (unit test mocks the failure path)
- Cannot handle S3 disk type "s3_plain" (design doc does not mention it; only "s3" and "object_storage")
- Cannot test against ClickHouse with actual S3 disk configuration without real infrastructure (integration tests deferred)
- Does not add `--skip-disks` / `--skip-disk-types` filtering (that is Phase 2d per roadmap)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Object disk metadata v5 (CH 25.10+) edge cases | YELLOW | Implement all 5 versions per spec; add unit tests for each version |
| CopyObject cross-region failure | YELLOW | Retry with exponential backoff (3 attempts); streaming fallback gated by `allow_object_disk_streaming` config; unit test for fallback path |
| Mixed disk parts in same table | GREEN | Per-part routing using existing `disk_types` HashMap |
| collect_parts signature change | GREEN | Single caller site; easy to update |
| UUID path derivation for restore | YELLOW | Match ClickHouse's `store/{3char}/{uuid_with_dashes}/` convention exactly |
| InlineData (v4) preservation | GREEN | Pass through untouched; no S3 copy needed for inline objects |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `info!("Object disk metadata parsed"` | yes | Confirms metadata parsing works on real files |
| `info!("S3 disk parts:.*CopyObject"` | yes | Confirms CopyObject pipeline active |
| `info!("Restoring S3 disk parts"` | yes | Confirms S3 restore path active |
| `warn!("CopyObject failed, falling back"` | no (conditional) | Only on cross-region failure |
| `ERROR` in object_disk module | no (forbidden) | Should not appear during normal operation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| `--skip-disks` / `--skip-disk-types` filtering | Phase 2d per roadmap | Phase 2d |
| Resume for S3 disk copies | Phase 2d covers all resume logic | Phase 2d |
| Manifest atomicity (tmp + CopyObject + delete) | Phase 2d | Phase 2d |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: Object disk metadata parser (src/object_disk.rs)
  - Task 2: S3Client copy_object methods with retry+backoff (src/storage/s3.rs)
  - Task 3: Concurrency helpers (src/concurrency.rs) -- TWO functions
  - Task 3b: Extend DiskRow with remote_path (src/clickhouse/client.rs)

Group B (Backup pipeline -- Sequential, depends on Group A):
  - Task 4: Disk-aware shadow walk (src/backup/collect.rs)
  - Task 5: Backup flow integration (src/backup/mod.rs)
  - Task 5b: Incremental diff s3_objects carry-forward (src/backup/diff.rs)

Group C (Upload -- depends on Group A + B):
  - Task 6: Mixed disk upload with CopyObject (src/upload/mod.rs)

Group D (Download -- depends on Group A):
  - Task 7: S3 disk download (metadata only) (src/download/mod.rs)

Group E (Restore -- Sequential, depends on Group A):
  - Task 8: UUID-isolated S3 restore with same-name optimization (src/restore/)

Group F (Wiring -- depends on ALL above):
  - Task 9: Wire module in lib.rs + integration smoke test
  - Task 10: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: Object Disk Metadata Parser

**Goal**: Create `src/object_disk.rs` with types and functions to parse all 5 ClickHouse object disk metadata format versions and serialize/rewrite metadata for restore.

**TDD Steps:**
1. Write failing test `test_parse_v1_absolute_paths` -- parse version 1 metadata with absolute S3 paths
2. Write failing test `test_parse_v2_relative_path` -- parse version 2 with relative paths
3. Write failing test `test_parse_v3_read_only_flag` -- parse version 3 with read_only flag
4. Write failing test `test_parse_v4_inline_data` -- parse version 4 with inline data (ObjectSize=0)
5. Write failing test `test_parse_v5_full_object_key` -- parse version 5 with full absolute key, extracting last 2 path components
6. Write failing test `test_rewrite_metadata_v2` -- rewrite a v2 metadata with new prefix, verify output
7. Write failing test `test_rewrite_metadata_v4_preserves_inline` -- rewrite v4, verify inline data preserved
8. Write failing test `test_serialize_roundtrip` -- parse then serialize, verify output matches expected format
9. Write failing test `test_is_s3_disk` -- test `is_s3_disk()` with "s3", "object_storage", "local" disk types
10. Implement `ObjectDiskMetadata`, `ObjectRef`, `parse_metadata()`, `rewrite_metadata()`, `serialize_metadata()`, `is_s3_disk()` to pass all tests

**Implementation Notes:**
- `ObjectDiskMetadata` struct: `version: u32, objects: Vec<ObjectRef>, total_size: u64, ref_count: u32, read_only: bool, inline_data: Option<String>`
- `ObjectRef` struct: `relative_path: String, size: u64`
- `parse_metadata(content: &str) -> Result<ObjectDiskMetadata>` -- parse text format per design 3.7
- `rewrite_metadata(metadata: &ObjectDiskMetadata, new_prefix: &str) -> String` -- generate new metadata text with updated paths, RefCount=0, ReadOnly=false
- `serialize_metadata(metadata: &ObjectDiskMetadata) -> String` -- serialize to text format
- `is_s3_disk(disk_type: &str) -> bool` -- returns true for "s3" or "object_storage" (per design 16.2)
- Version 5 (FullObjectKey): extract last 2 path components for relative path (design 3.7)
- InlineData (v4+): objects with size=0 have data inline; preserve during rewrite

**Files:** `src/object_disk.rs` (new)
**Acceptance:** F001

---

### Task 2: S3Client CopyObject Methods with Retry and Conditional Fallback

**Goal**: Add `copy_object()`, `copy_object_streaming()`, and `copy_object_with_retry()` to `S3Client` in `src/storage/s3.rs`. The retry wrapper implements exponential backoff per design §5.4 step 3d, and the streaming fallback is gated by `allow_object_disk_streaming` config per §12.

**TDD Steps:**
1. Write failing test `test_copy_object_builds_correct_source` -- verify CopyObject source format is `{bucket}/{key}` (unit test using mock or string verification)
2. Write failing test `test_copy_object_streaming_fallback` -- verify streaming path downloads then uploads
3. Write failing test `test_copy_object_with_retry_attempts` -- verify retry attempts 3 times with backoff before falling back
4. Write failing test `test_copy_object_with_retry_no_streaming_when_disabled` -- verify error propagated (not fallback) when `allow_object_disk_streaming = false`
5. Implement `copy_object()` -- uses AWS SDK `copy_object` API with `CopySource = "{source_bucket}/{source_key}"`
6. Implement `copy_object_streaming()` -- `get_object()` from source then `put_object()` to dest (fallback for cross-region)
7. Implement `copy_object_with_retry(&self, source_bucket, source_key, dest_key, allow_streaming: bool) -> Result<()>` -- retry `copy_object()` up to 3 times with exponential backoff (100ms, 400ms, 1600ms); on final failure, if `allow_streaming` is true, call `copy_object_streaming()` with a `warn!()` about high network traffic; if false, return the error
8. Verify all tests pass

**Implementation Notes:**
- `copy_object(&self, source_bucket: &str, source_key: &str, dest_key: &str) -> Result<()>` -- dest_key is relative to self's prefix; source is absolute (bucket+key)
- `copy_object_streaming(&self, source_bucket: &str, source_key: &str, dest_key: &str) -> Result<()>` -- download from source via raw AWS SDK client (not self.get_object since source may be different bucket), upload to dest via `self.put_object()`
- `copy_object_with_retry(&self, source_bucket: &str, source_key: &str, dest_key: &str, allow_streaming: bool) -> Result<()>` -- the method callers (upload Task 6, restore Task 8) should use; wraps retry logic + conditional streaming fallback
- Apply SSE and storage_class to the CopyObject request (same as put_object)
- CopySource format for AWS SDK: `"{bucket}/{key}"` (URL-encoded)
- For streaming fallback: need to construct a GetObject to the source bucket directly using `self.inner` (the underlying aws_sdk_s3::Client)
- When `allow_streaming = false` (default per config), CopyObject failure after retries is a hard error
- When `allow_streaming = true`, log `warn!("CopyObject failed after retries, falling back to streaming copy (high network traffic)")` per design §12

**Files:** `src/storage/s3.rs`
**Acceptance:** F002

---

### Task 3: Object Disk Copy Concurrency Helpers

**Goal**: Add two concurrency resolution functions to `src/concurrency.rs`: one for backup/upload CopyObject operations and one for restore CopyObject operations.

**TDD Steps:**
1. Write failing test `test_effective_object_disk_copy_concurrency` -- returns `config.backup.object_disk_copy_concurrency` (default 8)
2. Write failing test `test_effective_object_disk_server_side_copy_concurrency` -- returns `config.general.object_disk_server_side_copy_concurrency` (default 32)
3. Implement `effective_object_disk_copy_concurrency(config: &Config) -> u32` -- returns `config.backup.object_disk_copy_concurrency`; used by upload (Task 6)
4. Implement `effective_object_disk_server_side_copy_concurrency(config: &Config) -> u32` -- returns `config.general.object_disk_server_side_copy_concurrency`; used by restore (Task 8)
5. Verify both tests pass

**Implementation Notes:**
- These are **independent** concurrency settings, NOT a fallback chain:
  - `backup.object_disk_copy_concurrency` (default 8) -- bounds CopyObject during backup/upload
  - `general.object_disk_server_side_copy_concurrency` (default 32) -- bounds CopyObject during restore
- Design intent: backup CopyObject is conservative (8) since it runs alongside FREEZE; restore CopyObject is more aggressive (32) since it has the cluster to itself

**Files:** `src/concurrency.rs`
**Acceptance:** F003

---

### Task 3b: Extend DiskRow with remote_path

**Goal**: Add `remote_path` field to `DiskRow` in `src/clickhouse/client.rs` so that backup/upload/restore can determine the S3 source bucket and prefix for CopyObject operations.

**TDD Steps:**
1. Write failing test `test_disk_row_has_remote_path` -- verify `DiskRow` has `remote_path: String` field
2. Update `DiskRow` struct to add `pub remote_path: String` (with `#[serde(default)]` for backward compat)
3. Update `get_disks()` SQL query to include `remote_path` column: `SELECT name, path, type, ifNull(remote_path, '') as remote_path FROM system.disks`
4. Verify test passes and `cargo check` clean

**Implementation Notes:**
- `remote_path` is needed to determine the S3 source bucket path for CopyObject during backup. Without it, we'd have to guess.
- For local disks, `remote_path` will be empty string (via `ifNull(..., '')`)
- For S3 disks, `remote_path` contains the S3 URI or path prefix where ClickHouse stores data objects
- The `disk_map` in `backup/mod.rs` already maps disk name -> local path; we also need disk name -> remote path for S3 disks
- `#[serde(default)]` ensures deserialization from older ClickHouse versions that might not have the column

**Files:** `src/clickhouse/client.rs`
**Acceptance:** F003b

---

### Task 4: Disk-Aware Shadow Walk

**Goal**: Modify `collect_parts()` in `src/backup/collect.rs` to detect S3 disk parts and parse their metadata files instead of hardlinking data.

**TDD Steps:**
1. Write failing test `test_collect_parts_detects_s3_metadata` -- create a mock shadow directory with metadata files (no real S3 objects), verify parts returned with `s3_objects: Some(vec![...])` populated
2. Write failing test `test_collect_parts_local_disk_unchanged` -- verify local disk parts still have `s3_objects: None` and are hardlinked as before
3. Modify `collect_parts()` signature to accept `disk_type_map: &HashMap<String, String>` and `disk_paths: &HashMap<String, String>` parameters
4. Add logic: for each part directory, determine which disk it belongs to by matching the shadow path against disk paths
5. For S3 disk parts: read metadata files, call `parse_metadata()`, build `Vec<S3ObjectInfo>` from `ObjectRef` list, set `s3_objects: Some(...)` on `PartInfo`, skip hardlink
6. For local disk parts: existing hardlink behavior unchanged, `s3_objects: None`
7. Return collected parts keyed by actual disk name (not hardcoded "default")
8. Verify all tests pass

**Implementation Notes:**
- Signature change: `collect_parts(data_path: &str, freeze_name: &str, backup_dir: &Path, tables: &[TableRow], disk_type_map: &HashMap<String, String>, disk_paths: &HashMap<String, String>) -> Result<HashMap<String, Vec<(String, PartInfo)>>>`
- Return type change: value is now `Vec<(String, PartInfo)>` where String is the disk name, enabling proper grouping in mod.rs
  - Alternative: keep return as `HashMap<String, Vec<PartInfo>>` but change key to be `"{table_key}\x00{disk_name}"` -- too fragile
  - Better alternative: return `Vec<CollectedPart>` with `disk_name` field added to `CollectedPart`
- Add `disk_name: String` field to `CollectedPart` struct
- S3 disk detection: For each shadow path `{disk_path}/shadow/{freeze_name}/store/...`, check if `disk_path` matches an S3 disk in `disk_paths`+`disk_type_map`
- For S3 disk shadow directories: ClickHouse creates the shadow at `{s3_disk_path}/shadow/{freeze_name}/store/...`. The metadata files inside part directories describe which S3 objects belong to that part.
- Walk ALL disk paths (not just `data_path`): iterate `disk_paths`, for each disk whose shadow dir exists, walk it
- CRC64: For S3 disk parts, compute CRC64 of the checksums.txt file (same as local)
- Part size for S3 disk: sum of all `ObjectRef.size` values from parsed metadata
- **CRITICAL**: The existing call site in `backup/mod.rs:277` must be updated in Task 5

**Files:** `src/backup/collect.rs`
**Acceptance:** F004

---

### Task 5: Backup Flow Integration

**Goal**: Update `src/backup/mod.rs` to pass disk info to `collect_parts()` and group parts by actual disk name instead of hardcoded "default".

**TDD Steps:**
1. Verify compilation after updating `collect_parts()` call with new parameters
2. Write failing test `test_backup_groups_parts_by_disk_name` -- verify `TableManifest.parts` has keys matching actual disk names (e.g., "default" for local, "s3disk" for S3)
3. Update the `collect_parts()` call site to pass `disk_type_map` and `disk_map` (both already available in scope)
4. Replace hardcoded `parts_by_disk.insert("default".to_string(), ...)` with actual disk-name grouping from `CollectedPart.disk_name`
5. Populate `PartInfo.s3_objects` from the collected data
6. Verify test passes and `cargo check` is clean

**Implementation Notes:**
- `disk_map` and `disk_type_map` are already built at `backup/mod.rs:83-90` -- pass them into the spawned task
- The current code at line 291-293 does: `parts_by_disk.insert("default".to_string(), parts_for_table)` -- replace with grouping by `CollectedPart.disk_name`
- `CollectedPart` (from Task 4) now includes `disk_name` -- group by it to build `HashMap<String, Vec<PartInfo>>`
- `collect_parts` now returns data that includes disk names -- use it to build `parts_by_disk`
- Need to clone `disk_type_map` and `disk_map` into each spawned task (both are `HashMap<String, String>`, `Clone`)

**Files:** `src/backup/mod.rs`
**Acceptance:** F005

---

### Task 5b: Incremental Diff s3_objects Carry-Forward

**Goal**: Update `diff_parts()` in `src/backup/diff.rs` to carry forward `s3_objects` when a part is carried from the base manifest. Currently, only `backup_key` is copied; S3 disk parts also need their `s3_objects` list carried forward so the manifest remains self-contained for download/restore.

**TDD Steps:**
1. Write failing test `test_diff_parts_carries_s3_objects` -- base manifest has a part on "s3disk" with `s3_objects: Some(vec![S3ObjectInfo { path: "store/abc/data.bin", size: 100, backup_key: "chbackup/base/objects/data.bin" }])`, current has same part with `s3_objects: None` (just created by collect_parts). After `diff_parts()`, current part should have `s3_objects` copied from base.
2. Add one line to `diff_parts()` at the CRC64 match branch (line 58): `part.s3_objects = base_part.s3_objects.clone();`
3. Verify test passes

**Implementation Notes:**
- This is a one-line fix but it's critical for correctness: without it, carried S3 disk parts would lose their `s3_objects` list, making the manifest incomplete for restore
- The `s3_objects` from the base manifest contains the `backup_key` pointing to the original backup's S3 objects -- this is what restore needs to locate the data
- Per design §3.5: "Each part entry has: ... s3_objects" and carried parts must be self-contained
- Local disk parts have `s3_objects: None`, so cloning `None` is a no-op for them

**Files:** `src/backup/diff.rs`
**Acceptance:** F005b

---

### Task 6: Mixed Disk Upload with CopyObject

**Goal**: Modify `src/upload/mod.rs` to route S3 disk parts through CopyObject (with separate semaphore) instead of compress+upload.

**TDD Steps:**
1. Write failing test `test_upload_routes_s3_disk_to_copy` -- verify S3 disk parts use CopyObject path (mock/verify S3 key format differs)
2. Write failing test `test_upload_local_parts_unchanged` -- verify local disk parts still compress+upload
3. Add disk type detection: check `manifest.disk_types` to determine if a disk is S3
4. Split work items into two queues: `local_work_items` and `s3_disk_work_items`
5. For S3 disk parts: spawn tasks with separate `object_disk_copy_semaphore`, call `s3.copy_object()` for each `S3ObjectInfo` in `part.s3_objects`, then copy the metadata files
6. For local disk parts: existing compress+upload unchanged
7. Both queues run concurrently (two sets of spawned tasks, one `try_join_all`)
8. S3 disk parts set `backup_key` to the S3 key prefix in the backup bucket
9. Verify tests pass

**Implementation Notes:**
- **S3 object backup_key format** (per design §7.1): `{backup_name}/objects/{original_relative_path}`
  - Example: `chbackup/daily-2024-01-15/objects/store/abc/def/202401_1_50_3/data.bin`
  - Note: uses `objects/` prefix, NOT `data/{db}/{table}/`. The `data/` prefix is for local disk compressed archives only.
- **Part-level metadata backup_key**: `{backup_name}/data/{url_encode(db)}/{url_encode(table)}/{disk_name}/{part_name}/` (for the metadata files)
- Separate semaphore: `Arc::new(Semaphore::new(effective_object_disk_copy_concurrency(config)))` -- import from `crate::concurrency`
- **Source bucket/path**: determined from `DiskRow.remote_path` (added in Task 3b). Parse the S3 URI from `remote_path` to get source bucket and prefix. If `remote_path` is empty or same as backup bucket, use `config.s3.bucket`.
- **Destination**: `config.s3.object_disk_path` is the backup-side prefix for object disk data. If empty, defaults to same prefix as other backup data.
- For each S3 disk part with `s3_objects: Some(objects)`:
  - Each `S3ObjectInfo.path` is the relative path of the object
  - Source key: `{remote_path_prefix}/{relative_path}`
  - Dest key: `{backup_prefix}/{backup_name}/objects/{relative_path}`
  - After all objects copied, set `s3_obj.backup_key = dest_key`
- **CopyObject with retry+fallback**: call `s3.copy_object_with_retry(source_bucket, source_key, dest_key, config.s3.allow_object_disk_streaming)` from Task 2. This handles retry with backoff and conditional streaming fallback.
- No compression for S3 disk parts (already stored as raw S3 objects)
- Metadata files (the small text files from shadow) also need to be uploaded: `s3.put_object()` for each metadata file content

**Files:** `src/upload/mod.rs`
**Acceptance:** F006

---

### Task 7: S3 Disk Download (Metadata Only)

**Goal**: Modify `src/download/mod.rs` so S3 disk parts download only metadata files (not data objects) since data stays in the backup bucket until restore.

**TDD Steps:**
1. Write failing test `test_download_s3_disk_skips_data` -- verify S3 disk parts download metadata only, not full objects
2. Write failing test `test_download_local_parts_unchanged` -- verify local parts still full download+decompress
3. Add S3 disk detection: check `manifest.disk_types` for disk routing
4. For S3 disk parts with `s3_objects: Some(...)`: download only the part metadata (small files), save to local backup directory for restore to use later
5. For local disk parts: existing download+decompress unchanged
6. Verify tests pass

**Implementation Notes:**
- S3 disk parts: instead of downloading the full compressed archive, download only the metadata files stored during upload
- The metadata files are stored at the part-level prefix in the backup bucket
- Local directory structure for downloaded S3 metadata: `{backup_dir}/shadow/{db}/{table}/{part_name}/{metadata_files}`
- Each S3 disk part's `s3_objects` list describes the actual data objects (which stay on S3)
- The part's local metadata files are what get rewritten during restore

**Files:** `src/download/mod.rs`
**Acceptance:** F007

---

### Task 8: UUID-Isolated S3 Restore with Same-Name Optimization

**Goal**: Modify `src/restore/` to handle S3 disk parts by copying objects to UUID-derived paths, rewriting metadata, and implementing the same-name restore optimization per design §5.4 (explicitly required by Phase 2c roadmap).

**TDD Steps:**
1. Write failing test `test_restore_s3_uuid_path_derivation` -- verify UUID-based path generation: `store/{uuid_hex[0..3]}/{uuid_with_dashes}/`
2. Write failing test `test_restore_rewrite_metadata` -- verify metadata rewrite updates paths, sets RefCount=0, ReadOnly=false
3. Write failing test `test_restore_s3_parts_parallel_copy` -- verify S3 object copies use the object_disk_server_side_copy semaphore
4. Write failing test `test_restore_same_name_optimization` -- when restoring to same UUID, verify existing S3 objects (matching path+size) are skipped (zero-copy)
5. Extend `OwnedAttachParams` to include: `s3_client: Option<S3Client>`, `disk_type_map: HashMap<String, String>`, `object_disk_server_side_copy_concurrency: usize`, `allow_object_disk_streaming: bool`, `disk_remote_paths: HashMap<String, String>` (S3 disk remote paths for restore target)
6. In `attach_parts_owned()`: before sequential ATTACH, handle S3 disk parts:
   a. For parts where the disk is an S3 disk:
      - **Same-name optimization**: use `ListObjectsV2(prefix="store/{uuid_hex[0..3]}/{uuid_with_dashes}/")` to get existing S3 objects+sizes in one API call. Build a `HashMap<key, size>`. For each object in backup: if `original_path` exists in map AND sizes match → skip CopyObject (zero-copy). If missing → `copy_object_with_retry()`.
      - CopyObject all non-skipped `s3_objects` from backup bucket to data bucket at UUID-derived paths
   b. Rewrite metadata files to point to new UUID-derived paths
   c. Write rewritten metadata to `detached/{part_name}/`
   d. Chown metadata files
   e. Then proceed with normal `ATTACH PART`
7. For local disk parts: existing hardlink behavior unchanged
8. Verify tests pass

**Implementation Notes:**
- UUID path derivation per design §5.4: `store/{uuid_hex[0..3]}/{uuid_with_dashes}/{relative_path}`
  - The UUID comes from the destination table (assigned by ClickHouse at CREATE TABLE time)
  - Stored in `TableManifest.uuid` (populated during create, or queried from live tables during restore)
- `OwnedAttachParams` changes (add fields):
  - `s3_client: Option<S3Client>` -- needed for CopyObject during restore
  - `disk_type_map: HashMap<String, String>` -- to route parts by disk type
  - `object_disk_server_side_copy_concurrency: usize` -- semaphore size (from `effective_object_disk_server_side_copy_concurrency()`)
  - `allow_object_disk_streaming: bool` -- from `config.s3.allow_object_disk_streaming`; passed to `copy_object_with_retry()`
  - `disk_remote_paths: HashMap<String, String>` -- disk name -> remote_path for S3 disks (from `DiskRow.remote_path`)
- **Same-name optimization** (per design §5.4, required by Phase 2c roadmap):
  - Before copying objects, call `ListObjectsV2(prefix=store/{uuid_hex[0..3]}/{uuid_with_dashes}/)` on the data bucket
  - Build `existing_map: HashMap<String, u64>` (key → size)
  - For each object: if `existing_map[original_path]` matches size → skip (log `info!("Skipping existing S3 object")`)
  - This is a single ListObjectsV2 call per table, not per-object HeadObject
- S3 object copy: use `s3.copy_object_with_retry(source_bucket, source_key, dest_key, allow_streaming)` from Task 2
- Metadata rewrite: use `rewrite_metadata()` from Task 1 to update object paths and set RefCount=0, ReadOnly=false (per design §5.4 step 5)
- Write rewritten metadata files to `{table_data_path}/detached/{part_name}/` before ATTACH
- InlineData (v4+): no CopyObject needed for inline objects; preserve during rewrite
- Parallel S3 copies within a table: spawn tasks bounded by `object_disk_server_side_copy_concurrency` semaphore, then await all before proceeding to ATTACH

**Files:** `src/restore/mod.rs`, `src/restore/attach.rs`
**Acceptance:** F008

---

### Task 9: Wire Module and Integration Smoke Test

**Goal**: Add `pub mod object_disk` to `src/lib.rs` and write a basic integration test verifying the end-to-end flow compiles.

**TDD Steps:**
1. Add `pub mod object_disk;` to `src/lib.rs`
2. Run `cargo check` -- verify zero errors, zero warnings
3. Run `cargo test` -- verify all existing tests pass plus new unit tests from Tasks 1-8
4. Write a compile-time verification test that imports all new public types and functions

**Implementation Notes:**
- `pub mod object_disk;` added to `src/lib.rs` in alphabetical order (between `manifest` and `rate_limiter`)
- No runtime integration test (requires real ClickHouse + S3 disk setup)
- The smoke test verifies imports compile and types are usable

**Files:** `src/lib.rs`
**Acceptance:** F009

---

### Task 10: Update CLAUDE.md for All Modified Modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/backup, src/upload, src/download, src/restore, src/storage

**TDD Steps:**

1. **Read affected-modules.json for module list:**
   - src/backup/CLAUDE.md -- add disk-aware shadow walk, S3 disk part detection patterns
   - src/upload/CLAUDE.md -- add mixed disk upload pipeline, CopyObject path
   - src/download/CLAUDE.md -- add S3 disk metadata-only download
   - src/restore/CLAUDE.md -- add UUID-isolated S3 restore, metadata rewrite
   - src/storage/CLAUDE.md -- add copy_object and copy_object_streaming methods

2. **For each module, regenerate directory tree** using `tree -L 2` or `ls -la`

3. **Add new patterns to Key Patterns sections:**
   - backup: "Disk-Aware Shadow Walk" and "S3 Disk Part Detection"
   - upload: "Mixed Disk Upload Pipeline" and "CopyObject Concurrency"
   - download: "S3 Disk Metadata-Only Download"
   - restore: "UUID-Isolated S3 Restore" and "Metadata Rewrite"
   - storage: "CopyObject" and "Streaming Copy Fallback"

4. **Update Public API lists** for each module with new functions/methods

5. **Validate all CLAUDE.md files** have required sections: Parent Context, Directory Structure, Key Patterns, Parent Rules

**Files:** src/backup/CLAUDE.md, src/upload/CLAUDE.md, src/download/CLAUDE.md, src/restore/CLAUDE.md, src/storage/CLAUDE.md
**Acceptance:** FDOC

---

## Notes

### Phase 4.5 Skip Justification

Phase 4.5 (Interface Skeleton Simulation) is **partially applicable**. The new module `src/object_disk.rs` uses only standard library types and `anyhow::Result`. The S3Client methods use `aws_sdk_s3` types already in use. A full skeleton test is not needed since:
- All imports from the existing codebase are verified in `context/knowledge_graph.json`
- New types (`ObjectDiskMetadata`, `ObjectRef`) are self-contained structs with no external dependencies
- The `copy_object` method uses `aws_sdk_s3::Client` already imported in `s3.rs`

However, Task 9 serves as the compilation gate -- `cargo check` must pass with zero errors and zero warnings.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (cross-task types) | PASS | Task 1 produces `ObjectDiskMetadata` -> Task 4 consumes via `parse_metadata()` -> Task 8 consumes via `rewrite_metadata()`. Task 2 produces `copy_object_with_retry()` -> Tasks 6+8 consume. Task 3b `DiskRow.remote_path` -> Tasks 5,6,8 consume. |
| RC-016 (struct completeness) | PASS | `ObjectDiskMetadata` has all fields needed by consumers. `DiskRow` extended with `remote_path` for S3 source resolution. `OwnedAttachParams` extended with `disk_remote_paths` for restore source. |
| RC-017 (acceptance IDs) | PASS | F001-F009 + F003b + F005b + FDOC map to Tasks 1-10 + 3b + 5b. Each task references its acceptance ID. |
| RC-018 (dependencies) | PASS | Group B depends on A (Task 4 uses `parse_metadata` from Task 1). Group C depends on A+B (Task 6 uses `copy_object_with_retry` from Task 2, `DiskRow.remote_path` from Task 3b). Group E depends on A (Task 8 uses `rewrite_metadata`, `copy_object_with_retry`, concurrency). |
| RC-008 (TDD sequencing) | PASS | Task 5b uses `S3ObjectInfo` from manifest (existing). Task 6 uses `copy_object_with_retry()` from Task 2 (preceding group). Task 8 uses `ListObjectsV2` from existing S3Client + `rewrite_metadata()` from Task 1. |
| RC-019 (existing patterns) | PASS | Two concurrency helpers follow pattern of existing helpers. S3Client methods follow existing `put_object` pattern. Retry+backoff is new but standard. Parallel work queues follow upload/download pattern. |
| RC-021 (file locations) | PASS | All file locations verified: `S3Client` at `src/storage/s3.rs:25`, `collect_parts` at `src/backup/collect.rs:105`, `concurrency.rs` at root, `OwnedAttachParams` at `src/restore/attach.rs:45`, `DiskRow` at `src/clickhouse/client.rs:46`, `diff_parts` at `src/backup/diff.rs:29`. |
