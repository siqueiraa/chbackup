# Plan: Phase 2a -- Parallelism for chbackup

## Goal

Add parallel operations to all four command pipelines (create, upload, download, restore) using tokio semaphores, multipart S3 upload for large parts, and byte-level rate limiting. This converts the Phase 1 sequential-only tool into a production-grade parallel backup system matching the design doc sections 3.4, 3.6, 4, 5.3, and 11.1.

## Architecture Overview

Phase 2a introduces four flat semaphores (one per operation type) shared across all tables:

```
Operation          Semaphore                   What's parallel
---------          ---------                   ---------------
create (FREEZE)    max_connections             Tables
upload (S3 PUT)    upload_concurrency          Parts across ALL tables (flat)
download (S3 GET)  download_concurrency        Parts across ALL tables (flat)
restore (ATTACH)   max_connections             Tables
```

Key changes:
- **S3Client** gains multipart upload methods (CreateMultipartUpload, UploadPart, CompleteMultipartUpload, AbortMultipartUpload)
- **Rate limiter** module provides a token-bucket wrapper for byte streams
- **backup::create** parallelizes FREEZE+collect per table (bounded by max_connections)
- **upload::upload** flattens all parts into a single work queue (bounded by upload_concurrency)
- **download::download** same flat pattern (bounded by download_concurrency)
- **restore::restore** parallelizes tables (bounded by max_connections), with engine-aware ATTACH (sequential for Replacing/Collapsing, parallel for plain MergeTree)

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Config concurrency params**: Defined in `src/config.rs`, read by command modules
- **Arc<Semaphore>**: Created in each command's entry function, cloned into spawned tasks
- **S3Client**: Already `Clone` (src/storage/s3.rs:22) -- safe to clone into tasks
- **ChClient**: Already `Clone` (src/clickhouse/client.rs:12) -- safe to clone into tasks
- **FreezeGuard**: Owned by `backup::create`, per-task FreezeInfo collected after join
- **AttachParams**: Currently borrows (`&'a`) -- needs owned variant for `tokio::spawn`

### What This Plan CANNOT Do
- No streaming multipart (Phase 2a buffers compressed data, then decides single vs multipart)
- No S3 object disk support (Phase 2c)
- No incremental/diff-from (Phase 2b)
- No resume state tracking (Phase 2d)
- No parallel ATTACH within a single table for plain MergeTree (deferred -- tables parallel is sufficient for Phase 2a)

### Config Priority Resolution
The design doc references `general.upload_concurrency` in the concurrency table. However, the Go tool's behavior uses `backup.upload_concurrency` when set. Phase 2a uses `backup.*` fields as the primary concurrency control (already the convention in the config YAML), with `general.*` as a fallback only when `backup.*` is zero. This is consistent with Pattern 3 from context/patterns.md.

Resolved concurrency accessor pattern:
```rust
fn effective_upload_concurrency(config: &Config) -> u32 {
    if config.backup.upload_concurrency > 0 {
        config.backup.upload_concurrency
    } else {
        config.general.upload_concurrency
    }
}
```

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| FreezeGuard `&mut self` in parallel tasks | YELLOW | Per-task FreezeInfo collection, global guard for error cleanup |
| AttachParams lifetime references block tokio::spawn | YELLOW | Create OwnedAttachParams with owned String/PathBuf/Vec |
| Multipart upload leak on error | YELLOW | AbortMultipartUpload in scopeguard (drop guard pattern) |
| Race condition in manifest mutation | GREEN | Collect results after all tasks join, apply sequentially |
| Rate limiter accuracy under high concurrency | GREEN | Token bucket with atomic counter, acceptable jitter |
| Large number of spawned tasks for big backups | GREEN | Semaphore bounds actual concurrency; task overhead is minimal |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `INFO [upload] Uploading .* parts across .* tables (concurrency=\d+)` | yes | Upload start with concurrency info |
| `INFO [upload] Upload complete` | yes | Upload success |
| `INFO [create] Freezing .* tables (max_connections=\d+)` | yes | Parallel FREEZE start |
| `INFO [create] Backup .* created` | yes | Backup creation complete |
| `INFO [download] Downloading .* parts (concurrency=\d+)` | yes | Download start |
| `INFO [download] Download complete` | yes | Download success |
| `INFO [restore] Restoring .* tables (max_connections=\d+)` | yes | Restore start |
| `INFO [restore] Restore complete` | yes | Restore success |
| `DEBUG [upload] Part .* using multipart upload` | yes (for >32MB) | Multipart path exercised |
| `ERROR .*` | no (forbidden) | No errors during normal operation |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| url_encode duplication (4 copies) | Pre-existing tech debt | Future refactoring plan |
| compress_part duplication (upload + download) | Pre-existing tech debt | Future refactoring plan |
| Streaming multipart (no temp buffer) | Requires tokio_util codec pipeline | Phase 2 streaming improvements |
| S3 object disk support | Separate feature set | Phase 2c |
| Resume state tracking | Separate feature set | Phase 2d |

## Dependency Groups

```
Group A (Foundation -- Sequential):
  - Task 1: Add futures crate + concurrency helper module
  - Task 2: Add multipart upload methods to S3Client
  - Task 3: Add rate limiter module

Group B (Parallel Commands -- Independent, depends on Group A):
  - Task 4: Parallelize backup::create (FREEZE + collect)
  - Task 5: Parallelize upload::upload (flat work queue + multipart)
  - Task 6: Parallelize download::download (flat work queue)
  - Task 7: Parallelize restore::restore (tables parallel + engine-aware ATTACH)

Group C (Final -- Sequential, depends on Group B):
  - Task 8: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: Add futures crate and concurrency helper module

**Purpose:** Add the `futures` crate dependency and create a small `src/concurrency.rs` module that provides the effective concurrency accessors and common helper functions used by all parallel command modules.

**TDD Steps:**
1. Write failing test: `test_effective_upload_concurrency` -- verifies backup.upload_concurrency takes priority over general.upload_concurrency; when backup is 0, falls back to general
2. Write failing test: `test_effective_download_concurrency` -- same pattern for download
3. Write failing test: `test_effective_max_connections` -- returns clickhouse.max_connections (no override)
4. Implement `src/concurrency.rs` with:
   - `effective_upload_concurrency(config: &Config) -> u32`
   - `effective_download_concurrency(config: &Config) -> u32`
   - `effective_max_connections(config: &Config) -> u32`
5. Add `futures = "0.3"` to Cargo.toml `[dependencies]`
6. Add `pub mod concurrency;` to `src/lib.rs`
7. Verify all tests pass
8. Verify `cargo check` has 0 errors, 0 warnings

**Files:**
- `Cargo.toml` (add `futures = "0.3"`)
- `src/concurrency.rs` (new)
- `src/lib.rs` (add module declaration)

**Acceptance:** F001

**Implementation Notes:**
- The effective_* functions use `crate::config::Config` (verified in knowledge_graph.json)
- `Config.backup.upload_concurrency` is `u32` at `config.rs:341` (verified)
- `Config.general.upload_concurrency` is `u32` at `config.rs:59` (verified)
- `Config.clickhouse.max_connections` is `u32` at `config.rs:166` (verified)

---

### Task 2: Add multipart upload methods to S3Client

**Purpose:** Implement `create_multipart_upload`, `upload_part`, `complete_multipart_upload`, and `abort_multipart_upload` on `S3Client`. These are needed for uploading compressed parts larger than 32MB. Also add a high-level `upload_multipart` convenience method that orchestrates the full flow with abort-on-error cleanup.

**TDD Steps:**
1. Write unit test: `test_multipart_chunk_calculation` -- given uncompressed_size and s3 config, verify chunk size and part count are computed correctly (auto chunk_size when config is 0, respects max_parts_count)
2. Implement `S3Client::create_multipart_upload(&self, key: &str) -> Result<String>` returning upload_id
3. Implement `S3Client::upload_part(&self, key: &str, upload_id: &str, part_number: i32, body: Vec<u8>) -> Result<String>` returning ETag
4. Implement `S3Client::complete_multipart_upload(&self, key: &str, upload_id: &str, parts: Vec<(i32, String)>) -> Result<()>`
5. Implement `S3Client::abort_multipart_upload(&self, key: &str, upload_id: &str) -> Result<()>`
6. Implement `calculate_chunk_size(data_len: u64, config_chunk_size: u64, max_parts_count: u32) -> u64` as a standalone function
7. Write unit test: `test_calculate_chunk_size_auto` -- when config_chunk_size is 0, auto-computes from data_len / max_parts_count
8. Write unit test: `test_calculate_chunk_size_explicit` -- when config_chunk_size > 0, uses that value
9. Write unit test: `test_calculate_chunk_size_minimum` -- chunk size is at least 5MB (S3 minimum)
10. Verify `cargo check` has 0 errors, 0 warnings

**Files:**
- `src/storage/s3.rs` (add methods to S3Client impl block, add calculate_chunk_size fn)

**Acceptance:** F002

**Implementation Notes:**
- `aws_sdk_s3::Client` exposes `create_multipart_upload()`, `upload_part()`, `complete_multipart_upload()`, `abort_multipart_upload()` as fluent builders
- Must apply same SSE and storage_class as `put_object` (design consistency)
- `CompletedMultipartUpload` needs `CompletedPart` entries with part_number and e_tag
- S3 minimum part size is 5MB (except last part); maximum is 5GB
- `calculate_chunk_size` is a pure function -- easy to unit test without S3 connection
- Import `aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart}` for the complete call

---

### Task 3: Add rate limiter module

**Purpose:** Implement a token-bucket rate limiter that can be shared across all concurrent uploads/downloads. The rate limiter tracks bytes consumed and sleeps when the bucket is empty. When rate is 0, it is a no-op passthrough.

**TDD Steps:**
1. Write failing test: `test_rate_limiter_unlimited` -- when rate is 0, `consume(1000)` returns immediately (no delay)
2. Write failing test: `test_rate_limiter_basic` -- when rate is 100 bytes/sec, consuming 200 bytes takes ~2 seconds (within tolerance)
3. Write failing test: `test_rate_limiter_concurrent` -- two tasks sharing the same rate limiter both respect the global rate
4. Implement `src/rate_limiter.rs` with:
   - `RateLimiter::new(bytes_per_second: u64) -> Self` (0 = unlimited)
   - `RateLimiter::consume(&self, bytes: u64) -> impl Future<Output = ()>` (async, sleeps if needed)
   - Uses `Arc` internally so cloning shares state
5. Add `pub mod rate_limiter;` to `src/lib.rs`
6. Verify all tests pass
7. Verify `cargo check` has 0 errors, 0 warnings

**Files:**
- `src/rate_limiter.rs` (new)
- `src/lib.rs` (add module declaration)

**Acceptance:** F003

**Implementation Notes:**
- Token bucket implementation using `tokio::sync::Mutex<TokenBucketState>` where state tracks tokens and last_refill timestamp
- `tokio::time::sleep` for blocking when tokens depleted
- When bytes_per_second is 0, `consume` is a no-op (early return)
- The rate limiter is `Clone` (wraps `Arc<...>`) so it can be shared across spawned tasks
- No external crate needed -- hand-rolled token bucket is ~50 lines

---

### Task 4: Parallelize backup::create (FREEZE + collect)

**Purpose:** Convert the sequential `for table_row in &filtered_tables` loop in `backup::create` to parallel tasks bounded by `max_connections` semaphore. Each task: FREEZE -> collect_parts -> return (FreezeInfo, TableManifest). After all tasks join, aggregate results and UNFREEZE all.

**TDD Steps:**
1. Write unit test: `test_freeze_info_send_sync` -- verify FreezeInfo is Send + Sync (required for tokio::spawn)
2. Write unit test: `test_table_row_clone` -- verify TableRow can be cloned into spawned tasks
3. Implement parallel FREEZE+collect:
   - Create `Arc<Semaphore>` with `effective_max_connections(config)` permits
   - For each table: `tokio::spawn` a task that acquires permit, FREEZEs, collects parts, returns `(FreezeInfo, String/*full_name*/, TableManifest)`
   - Use `futures::future::try_join_all` on JoinHandle vec for fail-fast
   - On success: aggregate FreezeInfos into FreezeGuard, aggregate TableManifests
   - On error: UNFREEZE all successfully frozen tables before propagating error
4. Verify the existing `test_is_metadata_only_engine` still passes
5. Verify `cargo check` has 0 errors, 0 warnings

**Files:**
- `src/backup/mod.rs` (refactor create function)

**Acceptance:** F004

**Implementation Notes:**
- `ChClient` is `Clone` (verified: src/clickhouse/client.rs:12 has `#[derive(Clone)]`)
- `freeze_table` currently takes `&mut FreezeGuard` -- but in parallel, each task creates its own FreezeInfo and returns it; the FreezeGuard is assembled after join
- Replace direct `freeze_table(ch, &mut freeze_guard, ...)` with per-task `ch.freeze_table(db, table, freeze_name)` + returning FreezeInfo
- `all_tables` must be cloned into each spawn (for collect_parts); since it is `Vec<TableRow>` and `TableRow` is `Clone`, use `Arc<Vec<TableRow>>`
- The `backup_dir` must be cloned into each spawn (it is `PathBuf`)
- `config.clickhouse.data_path` and other config fields cloned into spawns
- Error cleanup: create a vec of FreezeInfos from successful tasks, UNFREEZE all on error
- `databases_seen` aggregated after join (no concurrent access needed)
- RC-008 check: Task 4 uses `effective_max_connections` from Task 1 -- preceding task

---

### Task 5: Parallelize upload::upload (flat work queue + multipart)

**Purpose:** Convert the sequential upload loop to a flat work queue where all parts across all tables are spawned through a single `upload_concurrency` semaphore. Parts with `size > 32MB` (uncompressed) use multipart upload. The rate limiter gates bytes uploaded.

**TDD Steps:**
1. Write unit test: `test_should_use_multipart` -- verify threshold logic: size > 32MB returns true, <= 32MB returns false
2. Write unit test: `test_upload_work_item_construction` -- verify work items collect db, table, part_name, part_dir, s3_key correctly from a manifest
3. Implement parallel upload:
   - Collect all (db, table, disk_name, PartInfo, part_dir, s3_key) work items into a Vec
   - Create `Arc<Semaphore>` with `effective_upload_concurrency(config)` permits
   - Create `RateLimiter::new(config.backup.upload_max_bytes_per_second)` (from Task 3)
   - For each work item: `tokio::spawn` async task that:
     a. Acquires semaphore permit
     b. `spawn_blocking` to compress part (existing `stream::compress_part`)
     c. If compressed.len() > multipart threshold: use `s3.upload_multipart(...)` with abort guard
     d. Else: use `s3.put_object(...)` (existing)
     e. `rate_limiter.consume(compressed.len())` after upload
     f. Returns `(table_key, disk_name, updated_part_info, compressed_size)`
   - `futures::future::try_join_all` for fail-fast
   - After join: apply all updated parts to manifest sequentially
   - Upload manifest last (existing logic)
4. Verify existing upload unit tests still pass
5. Verify `cargo check` has 0 errors, 0 warnings

**Files:**
- `src/upload/mod.rs` (refactor upload function)

**Acceptance:** F005

**Implementation Notes:**
- `S3Client` is `Clone` -- clone into each spawned task
- Multipart threshold: use `part.size > 32 * 1024 * 1024` (part.size is uncompressed u64, verified: manifest.rs:121)
- `s3_key_for_part` is a private function in upload/mod.rs -- reuse within same module
- `find_part_dir` is a private function in upload/mod.rs -- reuse within same module
- The `compressed` data (Vec<u8>) ownership transfers naturally into the upload call
- Manifest mutation: collect vec of `(table_key, disk_name, Vec<PartInfo>, u64)` from joins, then apply sequentially after all tasks complete (avoids concurrent HashMap mutation)
- Rate limiter: `consume(compressed.len() as u64).await` after each S3 put completes
- RC-015 check: spawn returns `Result<(String, String, Vec<PartInfo>, u64)>` -- consumer expects same types

---

### Task 6: Parallelize download::download (flat work queue)

**Purpose:** Convert the sequential download loop to a flat work queue where all parts across all tables are spawned through a single `download_concurrency` semaphore. Rate limiter gates bytes downloaded.

**TDD Steps:**
1. Write unit test: `test_download_work_item_construction` -- verify work items correctly extract db, table, part info from manifest
2. Implement parallel download:
   - Download manifest (existing, sequential -- small JSON)
   - Create local backup directory (existing, sequential)
   - Collect all (table_key, db, table, PartInfo) work items into a Vec
   - Create `Arc<Semaphore>` with `effective_download_concurrency(config)` permits
   - Create `RateLimiter::new(config.backup.download_max_bytes_per_second)`
   - For each work item: `tokio::spawn` async task that:
     a. Acquires semaphore permit
     b. Downloads compressed part from S3 (`s3.get_object`)
     c. `rate_limiter.consume(compressed.len())` after download
     d. `spawn_blocking` to decompress (`stream::decompress_part`)
     e. Returns `(table_key, compressed_size)`
   - `futures::future::try_join_all` for fail-fast
   - After join: tally totals
   - Save per-table metadata and manifest (existing logic, sequential)
3. Verify existing download unit tests still pass
4. Verify `cargo check` has 0 errors, 0 warnings

**Files:**
- `src/download/mod.rs` (refactor download function)

**Acceptance:** F006

**Implementation Notes:**
- `S3Client` is `Clone` -- clone into each spawned task
- `backup_dir` is `PathBuf` -- clone into each spawned task
- Each task needs owned `table_key: String`, `db: String`, `table: String`, `part: PartInfo`
- The `shadow_dir` computation and decompress target are per-task (no shared state)
- Per-table metadata save happens after all downloads complete (sequential pass)
- RC-008 check: Task 6 uses `effective_download_concurrency` from Task 1 -- preceding task

---

### Task 7: Parallelize restore::restore (tables parallel + engine-aware ATTACH)

**Purpose:** Parallelize table restore bounded by `max_connections`. Wire `needs_sequential_attach(engine)` into `attach_parts` to enable parallel ATTACH for plain MergeTree engines (deferred to Phase 2a+ if complexity is too high -- at minimum, tables run in parallel).

**TDD Steps:**
1. Write unit test: `test_owned_attach_params_send` -- verify OwnedAttachParams is Send (required for tokio::spawn)
2. Create `OwnedAttachParams` struct with owned fields:
   ```rust
   pub struct OwnedAttachParams {
       pub ch: ChClient,
       pub db: String,
       pub table: String,
       pub parts: Vec<PartInfo>,
       pub backup_dir: PathBuf,
       pub table_data_path: PathBuf,
       pub clickhouse_uid: Option<u32>,
       pub clickhouse_gid: Option<u32>,
       pub engine: String,
   }
   ```
3. Write unit test: `test_needs_sequential_attach_wired` -- verify that `needs_sequential_attach` is called in the attach flow and correctly routes MergeTree to parallel vs ReplacingMergeTree to sequential
4. Modify `attach_parts` to accept `OwnedAttachParams` (or keep `AttachParams` for the internal sequential path and have the caller handle dispatch)
5. Implement parallel restore:
   - Create `Arc<Semaphore>` with `effective_max_connections(config)` permits
   - For each table: `tokio::spawn` task that:
     a. Acquires semaphore permit
     b. Calls attach_parts with owned params
     c. Returns `(table_key, attached_count)`
   - `futures::future::try_join_all` for fail-fast
   - Aggregate totals after join
6. Verify existing restore unit tests still pass
7. Verify existing sort tests still pass (needs_sequential_attach already tested)
8. Verify `cargo check` has 0 errors, 0 warnings

**Files:**
- `src/restore/mod.rs` (refactor restore function)
- `src/restore/attach.rs` (add OwnedAttachParams, adapt attach_parts)

**Acceptance:** F007

**Implementation Notes:**
- `AttachParams<'a>` has lifetime refs (verified: src/restore/attach.rs:20) -- cannot cross tokio::spawn boundary
- Solution: create `OwnedAttachParams` with `String` instead of `&str`, `PathBuf` instead of `&Path`, `Vec<PartInfo>` instead of `&[PartInfo]`
- Keep the existing `AttachParams<'a>` as an internal type used within `attach_parts` for the sequential inner loop (no spawn boundary there)
- `needs_sequential_attach` already exists and is tested (src/restore/sort.rs:83) but NOT called in attach_parts -- must wire it in
- `ChClient` is `Clone` -- clone into each spawned task
- `engine` field added to `OwnedAttachParams` to enable the `needs_sequential_attach` check
- The `engine` for each table comes from `TableManifest.engine` (verified: manifest.rs:91)
- RC-008 check: Task 7 uses `effective_max_connections` from Task 1 -- preceding task
- RC-016 check: OwnedAttachParams has `engine` field needed by needs_sequential_attach

---

### Task 8: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/backup, src/upload, src/download, src/restore, src/storage

**TDD Steps:**

1. Read `context/affected-modules.json` for module list
2. For each module, regenerate directory tree with `tree -L 2 <module> --noreport`
3. Add new patterns for each module:
   - `src/backup/CLAUDE.md`: Add "Parallel FREEZE Pattern" section (semaphore, per-task FreezeInfo, aggregate after join)
   - `src/upload/CLAUDE.md`: Add "Parallel Upload Pattern" (flat work queue, multipart threshold, rate limiting)
   - `src/download/CLAUDE.md`: Add "Parallel Download Pattern" (flat work queue, rate limiting)
   - `src/restore/CLAUDE.md`: Add "Parallel Restore Pattern" (OwnedAttachParams, engine-aware ATTACH routing)
   - `src/storage/CLAUDE.md`: Add "Multipart Upload API" (create, upload_part, complete, abort)
4. Update root `CLAUDE.md` Phase 1 limitations section (remove parallelism limitation)
5. Validate all CLAUDE.md files have required sections: Parent Context, Directory Structure, Key Patterns, Parent Rules

**Files:**
- `src/backup/CLAUDE.md`
- `src/upload/CLAUDE.md`
- `src/download/CLAUDE.md`
- `src/restore/CLAUDE.md`
- `src/storage/CLAUDE.md`
- `CLAUDE.md` (root -- update status)

**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (cross-task types) | PASS | Task 4 returns (FreezeInfo, String, TableManifest); Task 5 returns (String, String, Vec<PartInfo>, u64) -- consumers match |
| RC-016 (struct completeness) | PASS | OwnedAttachParams in Task 7 has `engine` field needed by `needs_sequential_attach` |
| RC-017 (state field declared) | PASS | No new struct fields on existing types -- only new structs (OwnedAttachParams, RateLimiter) |
| RC-018 (TDD sequencing) | PASS | Task 1 (concurrency helpers) precedes Tasks 4-7 that use them; Task 2 (multipart) precedes Task 5 (upload) |
| RC-006 (verified APIs) | PASS | All method signatures verified against source (see context/symbols.md) |
| RC-008 (field ordering) | PASS | Each task references fields/functions from preceding tasks or existing codebase |
| RC-019 (existing patterns) | PASS | spawn_blocking for sync I/O, anyhow::Result returns, tracing logging -- all followed |

## Notes

### Phase 4.5 Skip Justification
Phase 4.5 (Interface Skeleton Simulation) is skipped because:
- All new public types (OwnedAttachParams, RateLimiter) are defined in this plan with clear fields
- No new imports from external crates beyond `futures` (which has well-known API)
- The `aws_sdk_s3` multipart types are well-documented and compilation is trivially verifiable
- The plan modifies existing public functions (same signatures), not creating new trait impls

### Multipart Threshold
Design doc says "parts where uncompressed_size > multipart_threshold (default 32MB)" but Phase 2a uses the simpler approach: compress first (already in-memory via spawn_blocking), then decide based on compressed size whether to use multipart. This avoids the complexity of predicting compressed size. The threshold is checked against `part.size` (uncompressed) to decide whether to potentially use multipart, but the actual multipart vs single-PUT decision can be made after compression.

### Parallel ATTACH Within a Table (Deferred)
The design doc mentions parallel ATTACH within a table for plain MergeTree engines. Phase 2a wires `needs_sequential_attach` to ensure sequential ATTACH for Replacing/Collapsing but keeps all ATTACH sequential within a table. This simplifies the initial implementation -- the primary parallelism gain comes from running multiple tables concurrently.
