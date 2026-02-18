# MR Review: Phase 2c -- S3 Object Disk Support

**Branch:** `claude/2026-02-18-01-phase2c-s3-object-disk`
**Base:** `master`
**Reviewer:** Claude (Codex unavailable)
**Date:** 2026-02-18
**Verdict:** **PASS**

---

## Phase 1: Automated Verification (12 checks)

| # | Check | Status | Details |
|---|-------|--------|---------|
| 1 | Compilation | PASS | `cargo check` -- zero errors, zero warnings |
| 2 | Test suite | PASS | 176 lib tests + 5 integration tests pass (181 total) |
| 3 | Debug markers | PASS | Zero `DEBUG_MARKER` / `DEBUG_VERIFY` markers in `src/` |
| 4 | Conventional commits | PASS | All 11 commits use `feat:`, `test:`, or `docs:` prefix |
| 5 | No AI mentions | PASS | No `claude`, `ai`, `chatgpt`, etc. in commit messages |
| 6 | Acceptance criteria | PASS | 12/12 features pass (F001-F009, F003b, F005b, FDOC) |
| 7 | New module wired | PASS | `pub mod object_disk;` in `src/lib.rs` (alphabetical order) |
| 8 | Config fields exist | PASS | `object_disk_copy_concurrency`, `object_disk_server_side_copy_concurrency`, `allow_object_disk_streaming` all present in `config.rs` with defaults and env overlay |
| 9 | Manifest schema | PASS | `disk_remote_paths`, `s3_objects` fields added with `#[serde(default, skip_serializing_if)]` for backward compat |
| 10 | CLAUDE.md updated | PASS | 5 module CLAUDE.md files updated (backup, upload, download, restore, storage) |
| 11 | No dead code warnings | PASS | Only one `#[allow(dead_code)]` on `DownloadWorkItem.disk_name` (reserved for future resume logic) |
| 12 | Root CLAUDE.md updated | PASS | Module map, data flow, limitations, and implementation status reflect Phase 2c |

---

## Phase 2: Design Review (6 areas)

### 2.1 Architecture Conformance

**PASS** -- Implementation follows the plan architecture precisely:

- **Object disk metadata parser** (`src/object_disk.rs`): Clean module with all 5 format versions implemented. Parse/serialize/rewrite functions match design doc section 3.7.
- **S3 CopyObject** (`src/storage/s3.rs`): Three-method API (`copy_object`, `copy_object_streaming`, `copy_object_with_retry`) follows the existing S3Client pattern. SSE and storage_class applied consistently.
- **Disk-aware backup** (`src/backup/collect.rs`): `collect_parts()` now accepts `disk_type_map` and `disk_paths`, walks ALL disk paths, routes S3 disk parts through metadata parsing.
- **Mixed disk upload** (`src/upload/mod.rs`): Two work queues with separate semaphores; S3 disk parts use CopyObject, local parts use compress+upload.
- **S3 disk download** (`src/download/mod.rs`): S3 disk parts download metadata only; data stays on S3.
- **UUID-isolated restore** (`src/restore/attach.rs`): CopyObject to UUID-derived paths, metadata rewrite, same-name optimization via single ListObjectsV2.

### 2.2 Error Handling

**PASS** -- Consistent `anyhow::Result` with `.context()` throughout:

- CopyObject retry with exponential backoff (100ms, 400ms, 1600ms) as specified.
- Streaming fallback gated by `allow_object_disk_streaming` config (default false = hard error).
- Metadata parse failures in `collect_s3_part_metadata` are logged at debug level and skipped (defensive: not all files in part dir are metadata).
- ListObjectsV2 failure in same-name optimization falls back to copying all objects (resilient).
- EXDEV cross-device fallback maintained for both local and S3 metadata file handling.

### 2.3 Concurrency Model

**PASS** -- Two independent concurrency paths as designed:

- **Local upload**: existing `upload_concurrency` semaphore (unchanged).
- **S3 disk CopyObject (upload)**: `effective_object_disk_copy_concurrency()` (default 8, conservative for alongside FREEZE).
- **S3 disk CopyObject (restore)**: `effective_object_disk_server_side_copy_concurrency()` (default 32, aggressive for standalone restore).
- Both queues run concurrently via `try_join_all` with fail-fast error propagation.
- S3 restore copies within a single table are parallelized with their own semaphore.

### 2.4 Data Flow Integrity

**PASS** -- Data flows correctly through all pipelines:

- `ObjectDiskMetadata` (Task 1) consumed by `collect_s3_part_metadata` (Task 4) and `rewrite_metadata` (Task 8).
- `DiskRow.remote_path` (Task 3b) flows through `disk_remote_paths` in manifest to upload (Task 6) and restore (Task 8).
- `CollectedPart.disk_name` enables proper per-disk grouping in `backup/mod.rs` (no more hardcoded "default").
- Incremental diff carries `s3_objects` forward (Task 5b) ensuring carried S3 parts remain self-contained.
- `S3ObjectInfo.backup_key` is set during upload and consumed during restore for CopyObject source.

### 2.5 Test Coverage

**PASS** -- Comprehensive unit tests for all new functionality:

- **object_disk.rs**: 19 tests covering all 5 metadata versions, roundtrip, rewrite, edge cases.
- **storage/s3.rs**: Tests for copy source format, retry with disabled streaming, mock client construction.
- **concurrency.rs**: Tests for both new helper functions with default and custom values.
- **clickhouse/client.rs**: Tests for `DiskRow.remote_path` and serde default behavior.
- **backup/collect.rs**: Tests for local disk unchanged, S3 disk metadata detection, `CollectedPart` struct.
- **backup/diff.rs**: Tests for s3_objects carry-forward and local parts unchanged.
- **upload/mod.rs**: Tests for S3 disk routing, source/dest key format, zero-size object skip, `parse_s3_uri`, metadata file collection.
- **download/mod.rs**: Tests for S3 disk detection, local parts not flagged, object_storage type.
- **restore/attach.rs**: Tests for UUID path derivation, metadata rewrite, `OwnedAttachParams` S3 fields.
- **lib.rs**: Compile-time verification tests for all Phase 2c public API.

### 2.6 Pattern Conformance

**PASS** -- New code follows established project patterns:

- Concurrency helpers follow the same pattern as existing `effective_upload_concurrency()`.
- S3Client methods follow existing `put_object`/`get_object` patterns (SSE, storage class applied).
- Parallel work queues follow the upload/download semaphore+try_join_all pattern.
- `OwnedAttachParams` extension follows the existing pattern of owned types for `tokio::spawn`.
- Config fields use `#[serde(default)]` with named default functions, consistent with existing config.
- URL encoding and path handling consistent with existing modules.
- Logging uses `tracing` macros (`info!`, `debug!`, `warn!`) consistently.

---

## Issues Found

### Critical

None.

### Important

None.

### Minor

| # | File | Line | Issue | Severity |
|---|------|------|-------|----------|
| M1 | `src/backup/collect.rs` | 330-331 | `disk_path_to_name` is built but only consumed by `let _ = &disk_path_to_name;` suppression. This unused map could be removed entirely. | minor |
| M2 | `src/upload/mod.rs` | 295 | `find_part_dir` for S3 disk parts may return a non-existent path without checking (existence check is only done for local parts at line 315). This is acceptable because S3 disk parts may not have local staging data, but the code path could be clearer. | minor |
| M3 | `src/restore/attach.rs` | 152 | `existing_map` uses `i64` for size (from `S3Object.size`) but compares against `u64` (from `S3ObjectInfo.size`). The cast at line 207 (`existing_size as u64`) is safe for positive values but could mask issues with malformed S3 responses returning negative sizes. | minor |

---

## Commit History Review

11 commits, all following conventional commit format:

1. `86d5ccb` - `feat(object_disk): add metadata parser for ClickHouse S3 object disk parts`
2. `b90f31f` - `feat(storage): add copy_object, copy_object_streaming, and copy_object_with_retry to S3Client`
3. `d59c38c` - `feat(concurrency): add object disk copy concurrency helpers`
4. `b33a546` - `feat(clickhouse): add remote_path field to DiskRow for S3 source resolution`
5. `83a5553` - `feat(backup): add disk-aware shadow walk and actual disk name grouping`
6. `786287b` - `feat(backup): carry forward s3_objects in incremental diff`
7. `97cb284` - `feat(upload): add mixed disk upload with CopyObject for S3 disk parts`
8. `1c758a5` - `feat(download): add S3 disk metadata-only download for object disk parts`
9. `f345a17` - `feat(restore): add UUID-isolated S3 restore with same-name optimization`
10. `9b15663` - `test(lib): add compile-time verification tests for Phase 2c public API`
11. `b7d410d` - `docs: update CLAUDE.md for Phase 2c S3 object disk support`

Commits follow task dependency ordering (Group A -> B -> C/D/E -> F). Each commit is self-contained and compilable.

---

## Statistics

| Metric | Value |
|--------|-------|
| Files changed | 20 |
| Lines added | 3,057 |
| Lines removed | 254 |
| New module | `src/object_disk.rs` (568 lines) |
| Tests added | ~95 new test cases |
| Total test count | 176 lib + 5 integration = 181 |
| Compiler warnings | 0 |
| Debug markers | 0 |

---

## Summary

Phase 2c adds comprehensive S3 object disk support across all four command pipelines (create, upload, download, restore). The implementation is well-structured, follows existing codebase patterns, and includes thorough unit test coverage. All 12 acceptance criteria pass. The three minor issues identified are cosmetic and do not affect correctness or performance.

**Recommendation:** Merge.
