# References — Phase 2a Parallelism

**Date**: 2026-02-17

## Symbol Analysis

### Core Functions Being Modified

#### 1. `backup::create` (src/backup/mod.rs:37)
**Signature**: `pub async fn create(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool) -> Result<BackupManifest>`
**Callers**: `src/main.rs:159` (single call site)
**Current behavior**: Sequential loop over `filtered_tables` — freezes one table at a time, collects parts sequentially.
**Phase 2a change**: Parallel FREEZE via `tokio::spawn` + `Arc<Semaphore>` bounded by `max_connections`. Each table task: FREEZE -> collect parts -> UNFREEZE. FreezeGuard needs per-task scoping (scopeguard pattern on each spawned task).

#### 2. `upload::upload` (src/upload/mod.rs:66)
**Signature**: `pub async fn upload(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool) -> Result<()>`
**Callers**: `src/main.rs:191` (single call site)
**Current behavior**: Sequential loop over all tables, then parts within each table. Compresses each part via `spawn_blocking`, then calls `s3.put_object()` sequentially.
**Phase 2a change**: Flat work queue of all parts across all tables. `tokio::spawn` each through `Arc<Semaphore>(upload_concurrency)`. Parts > 32MB uncompressed use multipart upload. `try_join_all` for fail-fast.

#### 3. `download::download` (src/download/mod.rs:49)
**Signature**: `pub async fn download(config: &Config, s3: &S3Client, backup_name: &str) -> Result<PathBuf>`
**Callers**: `src/main.rs:211` (single call site)
**Current behavior**: Sequential loop over tables and parts. Downloads each part from S3, decompresses via `spawn_blocking`.
**Phase 2a change**: Same flat work queue pattern as upload. `Arc<Semaphore>(download_concurrency)`. Metadata download phase is unbounded (small JSON). Data phase through semaphore.

#### 4. `restore::restore` (src/restore/mod.rs:43)
**Signature**: `pub async fn restore(config: &Config, ch: &ChClient, backup_name: &str, table_pattern: Option<&str>, schema_only: bool, data_only: bool) -> Result<()>`
**Callers**: `src/main.rs:267` (single call site)
**Current behavior**: Sequential loop over `table_keys`. Calls `attach_parts()` for each table sequentially.
**Phase 2a change**: Tables in parallel bounded by `max_connections`. Within each table: sequential sorted ATTACH for Replacing/Collapsing engines, parallel ATTACH for plain MergeTree.

#### 5. `restore::attach::attach_parts` (src/restore/attach.rs:43)
**Signature**: `pub async fn attach_parts(params: &AttachParams<'_>) -> Result<u64>`
**Callers**: `src/restore/mod.rs:178` (single call site)
**Current behavior**: Sequential loop over `sorted_parts`, hardlinks and ATTACHes one by one.
**Phase 2a change**: Needs engine-aware branching: if `needs_sequential_attach(engine)` -> keep sequential sorted. Else -> parallel ATTACH (bounded).

### Key Types

#### `FreezeGuard` (src/backup/freeze.rs:23)
```rust
pub struct FreezeGuard {
    frozen: Vec<FreezeInfo>,
}
```
**Methods**: `new()`, `add()`, `frozen_tables()`, `len()`, `is_empty()`, `unfreeze_all(ch)`
**Drop**: Warns if frozen tables remain
**Phase 2a impact**: For parallel FREEZE, each spawned task needs its own FreezeInfo or a shared Arc<Mutex<FreezeGuard>>. Design doc says scopeguard UNFREEZE on every task — so per-task FreezeInfo is preferred. The global FreezeGuard can collect all after tasks complete for error-path cleanup.

#### `AttachParams` (src/restore/attach.rs:20)
```rust
pub struct AttachParams<'a> {
    pub ch: &'a ChClient,
    pub db: &'a str,
    pub table: &'a str,
    pub parts: &'a [PartInfo],
    pub backup_dir: &'a Path,
    pub table_data_path: &'a Path,
    pub clickhouse_uid: Option<u32>,
    pub clickhouse_gid: Option<u32>,
}
```
**Phase 2a impact**: Needs to be usable across `tokio::spawn` boundaries. References (`&'a`) prevent moving into spawn. Will need to own data or use `Arc`.

#### `S3Client` (src/storage/s3.rs:23)
```rust
#[derive(Clone, Debug)]
pub struct S3Client { ... }
```
**Phase 2a impact**: Already `Clone` and `Debug`, so can be cloned into spawned tasks. Uses `aws_sdk_s3::Client` internally which is also `Clone` (reference-counted).

#### `ChClient` (src/clickhouse/client.rs:12)
```rust
#[derive(Clone)]
pub struct ChClient { ... }
```
**Phase 2a impact**: Already `Clone`, can be shared across spawned tasks. Uses `clickhouse::Client` internally which also supports concurrent use.

### Config Parameters Relevant to Parallelism

| Field | Type | Default | Location | Purpose |
|-------|------|---------|----------|---------|
| `general.upload_concurrency` | u32 | 4 | src/config.rs:59 | Semaphore permits for S3 uploads |
| `general.download_concurrency` | u32 | 4 | src/config.rs:63 | Semaphore permits for S3 downloads |
| `general.upload_max_bytes_per_second` | u64 | 0 | src/config.rs:67 | Rate limit (0 = unlimited) |
| `general.download_max_bytes_per_second` | u64 | 0 | src/config.rs:71 | Rate limit (0 = unlimited) |
| `clickhouse.max_connections` | u32 | 1 | src/config.rs:166 | Semaphore for FREEZE/restore tables |
| `backup.upload_concurrency` | u32 | 4 | src/config.rs:341 | Alternative upload concurrency |
| `backup.download_concurrency` | u32 | 4 | src/config.rs:345 | Alternative download concurrency |
| `backup.upload_max_bytes_per_second` | u64 | 0 | src/config.rs:352 | Alternative rate limit |
| `backup.download_max_bytes_per_second` | u64 | 0 | src/config.rs:356 | Alternative rate limit |
| `s3.max_parts_count` | u32 | 10000 | src/config.rs:298 | S3 multipart max parts |
| `s3.chunk_size` | u64 | 0 | src/config.rs:302 | S3 multipart chunk size (0 = auto) |
| `s3.concurrency` | u32 | 1 | src/config.rs:306 | S3 SDK internal concurrency per upload |

**Important**: There are TWO sets of concurrency/rate-limit fields: `general.*` and `backup.*`. The design doc and Go tool use `general.upload_concurrency` as the primary control. Need to determine which takes priority (likely `backup.*` overrides `general.*` or they serve different contexts).

### `needs_sequential_attach` (src/restore/sort.rs:83)
```rust
pub fn needs_sequential_attach(engine: &str) -> bool {
    engine.contains("Replacing") || engine.contains("Collapsing") || engine.contains("Versioned")
}
```
**Status**: Defined and tested, but NOT YET CALLED in production code. Phase 2a must wire this into the restore logic for engine-aware ATTACH behavior.

### S3 Multipart Upload Methods

The current `S3Client` (src/storage/s3.rs) has:
- `put_object(key, body: Vec<u8>)` - single PutObject, body must be in memory
- `put_object_with_options(key, body: Vec<u8>, content_type)` - with SSE/storage class
- `get_object(key) -> Vec<u8>` - full download to memory
- `get_object_stream(key) -> ByteStream` - streaming download (exists but unused)

**Missing for Phase 2a**:
- `create_multipart_upload(key) -> upload_id`
- `upload_part(key, upload_id, part_number, body) -> ETag`
- `complete_multipart_upload(key, upload_id, parts)`
- `abort_multipart_upload(key, upload_id)` - for scopeguard cleanup

### `compress_part` (src/upload/stream.rs:16 and src/download/stream.rs:36)
```rust
pub fn compress_part(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>>
```
**Note**: This function exists in BOTH `upload/stream.rs` and `download/stream.rs` (duplicate). Phase 2a should consolidate. Both produce in-memory `Vec<u8>`. For multipart upload, might need streaming variant.

### `decompress_part` (src/download/stream.rs:16)
```rust
pub fn decompress_part(data: &[u8], output_dir: &Path) -> Result<()>
```
**Phase 2a impact**: Still sync, still in spawn_blocking. No change needed for parallelism.

## Cross-Reference: Design Doc to Code

| Design Section | Code Location | Status | Phase 2a Need |
|---------------|---------------|--------|----------------|
| 11.1 Flat semaphores | Not implemented | Missing | Add `Arc<Semaphore>` per operation type |
| 3.4 Parallel FREEZE | src/backup/mod.rs:141 (sequential) | Sequential | Convert to spawned tasks |
| 3.6 Parallel upload | src/upload/mod.rs:107 (sequential) | Sequential | Flat work queue + semaphore |
| 3.6 Multipart upload | src/storage/s3.rs (PutObject only) | Missing | Add multipart APIs |
| 3.6 Rate limiting | Not implemented | Missing | Token bucket on byte streams |
| 4 Parallel download | src/download/mod.rs:89 (sequential) | Sequential | Same pattern as upload |
| 5.3 Parallel restore | src/restore/mod.rs:137 (sequential) | Sequential | Tables parallel, engine-aware ATTACH |
| 5.3 needs_sequential | src/restore/sort.rs:83 | Exists, unwired | Wire into attach_parts |

## Preventive Rules Applied

### RC-006: Verified APIs
All method signatures above were verified by reading actual source code, not assumed from memory.

### RC-021: Struct locations verified
- `FreezeGuard` -> src/backup/freeze.rs:23 (verified)
- `AttachParams` -> src/restore/attach.rs:20 (verified)
- `S3Client` -> src/storage/s3.rs:23 (verified)
- `ChClient` -> src/clickhouse/client.rs:12 (verified)
- Config fields -> src/config.rs (verified line numbers above)

### RC-019: Existing patterns
- `tokio::task::spawn_blocking` is the established pattern for sync I/O (collect.rs:186, upload/mod.rs:156, download/mod.rs:142)
- All command entry points use `async fn` and return `Result<_>`
- S3Client and ChClient are both `Clone`, enabling sharing across tasks
