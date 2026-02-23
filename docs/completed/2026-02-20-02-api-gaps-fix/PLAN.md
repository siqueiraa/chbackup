# Plan: Fix Implementation Gaps Found in Audit of chbackup API

## Goal

Fix six implementation gaps identified during an audit of the chbackup API: a stub `post_actions` handler that does not dispatch commands, missing pagination/format on the list endpoint, hardcoded-zero `object_disk_size` and empty `required` fields in list responses, missing SIGTERM handler for Kubernetes graceful shutdown, and documentation errors in CLAUDE.md and docs/design.md.

## Architecture Overview

All changes touch the HTTP API layer (`src/server/routes.rs`, `src/server/mod.rs`) and the backup listing module (`src/list.rs`). The `BackupSummary` struct in `list.rs` gains two new fields (`object_disk_size`, `required`) that flow through to `ListResponse` via `summary_to_list_response()`. The `post_actions` handler is upgraded from a no-op stub to a real command dispatcher following existing route handler patterns. The list endpoint gains pagination (offset/limit) and format support following the tables endpoint pattern. A SIGTERM handler is added to `server/mod.rs` to enable Kubernetes graceful shutdown. Documentation files are corrected.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **BackupSummary**: Defined in `src/list.rs:46`. Constructed by `parse_backup_summary()` (local) and `list_remote()` (remote). Consumed by `summary_to_list_response()` in `src/server/routes.rs:321`.
- **ListParams**: Defined in `src/server/routes.rs:65`. Deserialized from query params by axum in `list_backups()`.
- **ListResponse**: Defined in `src/server/routes.rs:73`. Returned by `list_backups()` and consumed by ClickHouse integration tables.
- **post_actions**: Defined in `src/server/routes.rs:217`. Registered in `build_router()` at `src/server/mod.rs:50`.
- **Signal handlers**: Registered in `start_server()` at `src/server/mod.rs`.

### What This Plan CANNOT Do
- Cannot add runtime debug markers that require a running ClickHouse + S3 instance (no infra available in CI). Runtime verification relies on compilation + unit tests.
- Cannot change the `BackupManifest` schema (no `required_backups` field exists in manifests; the `required` field is derived by scanning `PartInfo.source` for `"carried:{base}"` prefixes).
- Cannot test signal handlers in unit tests (requires process-level signal delivery).

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| BackupSummary field addition breaks many test sites (~25) | YELLOW | All construction sites identified in context/references.md. Use `#[serde(default)]` for backward compat. Mechanical update. |
| post_actions dispatch uses default params only | GREEN | Design doc confirms default params are acceptable for POST /api/v1/actions (Go parity). Each command dispatches with sensible defaults matching existing handlers. |
| SIGTERM handler race with SIGINT handler | GREEN | Both trigger the same shutdown logic via `tokio::signal::ctrl_c()` pattern replacement. Only one fires. |
| List endpoint return type change | GREEN | Exact pattern already proven in tables() endpoint. |

## Expected Runtime Logs

This plan does NOT add debug markers because:
1. No runtime binary can be executed (no ClickHouse/S3 infra).
2. All changes are verifiable via compilation + unit tests.
3. The existing tracing logs already cover the modified paths.

| Pattern | Required | Description |
|---------|----------|-------------|
| `Action dispatched from POST /api/v1/actions` | existing | Already logged by post_actions, will now appear with actual dispatch |
| `Starting {command} operation` | existing | Logged by each command handler |
| `Shutdown signal received` | existing | Logged by SIGINT handler, will also fire on SIGTERM |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| `data_size` = `size - object_disk_size` computation | Requires BackupSummary to have object_disk_size first (this plan adds it); final wire-up is trivial but deferred to avoid scope creep | Next patch after this plan merges |
| SIGTERM handler in main.rs for standalone watch | Similar to server handler but different shutdown mechanism | Can be a follow-up 1-task plan |
| ManifestCache TTL for list format output | API format output only useful for text/csv/tsv consumers; JSON is default | Low priority |

## Dependency Groups

```
Group A (Independent - Server routes fixes):
  - Task 1: Fix post_actions stub to dispatch commands (routes.rs)
  - Task 2: Add offset/limit/format params to list endpoint (routes.rs)

Group B (Sequential - BackupSummary enrichment):
  - Task 3: Add object_disk_size and required fields to BackupSummary (list.rs)
  - Task 4: Wire BackupSummary fields into ListResponse (routes.rs) -- depends on Task 3

Group C (Independent - Signal handling):
  - Task 5: Add SIGTERM handler to server/mod.rs

Group D (Independent - Documentation fixes):
  - Task 6: Fix CLAUDE.md documentation errors
  - Task 7: Fix docs/design.md section 7.1 errors

Group E (Sequential - Final):
  - Task 8: Update CLAUDE.md for all modified modules (MANDATORY) -- depends on all above
```

## Tasks

### Task 1: Fix post_actions stub to dispatch commands

**Goal:** Replace the no-op stub in `post_actions()` (routes.rs:248-253) with actual command dispatch following the existing handler delegation pattern.

**TDD Steps:**
1. Write test `test_post_actions_unknown_command` -- send unknown command, expect 400 BAD_REQUEST (already works, regression test).
2. Write test `test_post_actions_empty_body` -- send empty Vec, expect 400 BAD_REQUEST (already works, regression test).
3. Implement: Replace the stub `tokio::spawn` block (lines 248-254) with a `match` on `op_name` that dispatches to the same command functions called by existing handlers. Each branch extracts `backup_name` from `parts.get(1)` (second word), defaults to UTC timestamp if missing. Use default params (no tables filter, no diff_from, no partitions, etc.).
4. Write test `test_post_actions_valid_commands` -- verify each recognized command string is accepted (returns 200 with OperationStarted). Note: actual execution happens in background task, so we only test the dispatch path.
5. Verify `cargo test` passes.
6. Verify `cargo clippy` has no warnings.

**Implementation Notes:**
The dispatch match arms should follow these patterns (simplified):
- `"create"`: mirrors `create_backup()` at routes.rs:342 -- calls `crate::backup::create()` with default params
- `"upload"`: mirrors `upload_backup()` at routes.rs:434 -- calls `crate::upload::upload()` with default params
- `"download"`: mirrors `download_backup()` at routes.rs:517 -- calls `crate::download::download()` with default params
- `"restore"`: mirrors `restore_backup()` at routes.rs:584 -- calls `crate::restore::restore()` with default params
- `"create_remote"`: mirrors `create_remote()` at routes.rs:693 -- calls create then upload
- `"restore_remote"`: mirrors `restore_remote()` at routes.rs:822 -- calls download then restore
- `"delete"`: mirrors `delete_backup()` at routes.rs:956 -- needs location (second word) and name (third word). Default to remote if only name given.
- `"clean_broken"`: mirrors `clean_remote_broken()` at routes.rs:1039 + `clean_local_broken()` at routes.rs:1100

Each arm must:
- Load config/ch/s3 via `state_clone.config.load()`, `state_clone.ch.load()`, `state_clone.s3.load()`
- Record duration via `std::time::Instant::now()`
- Call `finish_op(id)` on success, `fail_op(id, err)` on failure
- Update metrics (duration, success/error counters) following existing handler pattern

**Files:** `src/server/routes.rs`
**Acceptance:** F001

---

### Task 2: Add offset/limit/format params to list endpoint

**Goal:** Add pagination (offset/limit with X-Total-Count header) and format query param to the `/api/v1/list` endpoint, following the exact pattern from the tables() endpoint at routes.rs:1495-1525.

**TDD Steps:**
1. Write test `test_list_params_deserialization` -- verify ListParams deserializes offset, limit, format from query string (follow `test_tables_params_deserialization` pattern at routes.rs:1941).
2. Implement: Add `offset: Option<usize>`, `limit: Option<usize>`, `format: Option<String>` fields to `ListParams` struct.
3. Implement: Change `list_backups()` return type from `Result<Json<Vec<ListResponse>>, ...>` to `Result<([(HeaderName, HeaderValue); 1], Json<Vec<ListResponse>>), ...>`.
4. Implement: After building results and applying desc sort, apply pagination (skip/take) and add X-Total-Count header. Copy exact pattern from tables() at routes.rs:1495-1525.
5. Note: The `format` param is parsed but only used if the caller wants non-JSON output. For API responses, JSON is always the format (axum Json wrapper). The `format` field is stored in ListParams for potential future use or for integration table DDL compatibility, but the handler always returns JSON.
6. Verify `cargo test` passes.
7. Verify `cargo clippy` has no warnings.

**Files:** `src/server/routes.rs`
**Acceptance:** F002

---

### Task 3: Add object_disk_size and required fields to BackupSummary

**Goal:** Add `object_disk_size: u64` and `required: String` fields to `BackupSummary` in `src/list.rs`. Compute `object_disk_size` by summing `s3_objects[].size` across all parts where the disk type is an S3 disk (using `is_s3_disk()`). Extract `required` by finding the first `"carried:{base}"` source in manifest parts.

**TDD Steps:**
1. Write test `test_backup_summary_object_disk_size` -- create a BackupSummary with object_disk_size set, verify field access.
2. Write test `test_extract_required_from_manifest` -- construct a BackupManifest with one part having `source: "carried:base-backup"`, verify extraction logic returns `"base-backup"`.
3. Write test `test_extract_required_empty` -- construct a BackupManifest with all parts having `source: "uploaded"`, verify extraction returns empty string.
4. Implement: Add `pub object_disk_size: u64` and `pub required: String` fields to `BackupSummary` with `#[serde(default)]` for backward compat.
5. Implement: Add private helper `fn compute_object_disk_size(manifest: &BackupManifest) -> u64` that iterates `manifest.tables.values()` -> `table.parts.values()` -> for each part, if `part.s3_objects.is_some()`, sum all `s3_obj.size`. This is simpler than checking `disk_types` because `s3_objects` is only populated for S3 disk parts.
6. Implement: Add private helper `fn extract_required_backup(manifest: &BackupManifest) -> String` that iterates manifest parts and returns the first `source.strip_prefix("carried:")` match, or empty string.
7. Update `parse_backup_summary()` OK path (line 1204): add `object_disk_size: compute_object_disk_size(&manifest)` and `required: extract_required_backup(&manifest)`.
8. Update `list_remote()` OK path (line 394): add same computation.
9. Update all broken-backup construction sites (6 total): set `object_disk_size: 0, required: String::new()`.
10. Update all test construction sites (~25 total): add `object_disk_size: 0, required: String::new()`.
11. Verify `cargo test` passes.
12. Verify `cargo clippy` has no warnings.

**Implementation Notes:**
- `compute_object_disk_size()` does NOT need `is_s3_disk()` or `disk_types` because `PartInfo.s3_objects` is `Option<Vec<S3ObjectInfo>>` and is only `Some(...)` for S3 disk parts. So we simply sum all `s3_objects[].size` across all parts. This is the authoritative source (verified in context/data-authority.md).
- `extract_required_backup()` follows the exact `strip_prefix("carried:")` pattern from `collect_incremental_bases()` at list.rs:959 but operates on a single already-loaded manifest (sync, no S3 access needed).
- Broken backups have no manifest data, so both new fields default to `0` / empty string.

**Files:** `src/list.rs`, `src/lib.rs` (test), `src/watch/mod.rs` (test helper)
**Acceptance:** F003

---

### Task 4: Wire BackupSummary fields into ListResponse

**Goal:** Update `summary_to_list_response()` in routes.rs to read `object_disk_size` and `required` from `BackupSummary` instead of hardcoding 0 and empty string.

**TDD Steps:**
1. Update existing test `test_summary_to_list_response_sizes` (routes.rs:2124) to set `object_disk_size: 512` and `required: "base-backup".to_string()` on the BackupSummary, then assert `response.object_disk_size == 512` and `response.required == "base-backup"`.
2. Implement: In `summary_to_list_response()` (routes.rs:321-335), change `object_disk_size: 0` to `object_disk_size: s.object_disk_size` and `required: String::new()` to `required: s.required`.
3. Verify `cargo test` passes.
4. Verify `cargo clippy` has no warnings.

**Files:** `src/server/routes.rs`
**Acceptance:** F004

---

### Task 5: Add SIGTERM handler to server/mod.rs

**Goal:** Add a SIGTERM handler in `start_server()` so that `kill <pid>` and Kubernetes `kubectl delete pod` trigger graceful shutdown, identical to SIGINT/Ctrl+C behavior.

**TDD Steps:**
1. No unit test possible for signal handlers (requires process-level signal delivery).
2. Implement: In `start_server()`, replace the two `tokio::signal::ctrl_c()` calls (TLS path at line 303, plain path at line 334) with a helper that resolves when EITHER SIGINT or SIGTERM is received.
3. The implementation approach: Create a local async function `shutdown_signal()` that uses `tokio::select!` to wait for either `tokio::signal::ctrl_c()` or `signal(SignalKind::terminate()).recv()` (on Unix). On non-Unix, just `ctrl_c()`.
4. Replace `tokio::signal::ctrl_c().await.ok()` at line 303 with `shutdown_signal().await`.
5. Replace the `with_graceful_shutdown` closure at line 333-344 to use the same `shutdown_signal()`.
6. Verify `cargo check` passes (compilation).
7. Verify `cargo clippy` has no warnings.

**Implementation Notes:**
The shutdown_signal helper pattern:
```rust
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate()
        ).expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = term.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}
```

This follows the existing signal handler pattern (SIGHUP at line 217, SIGQUIT at line 239) but for shutdown rather than reload/dump.

**Files:** `src/server/mod.rs`
**Acceptance:** F005

---

### Task 6: Fix CLAUDE.md documentation errors

**Goal:** Correct four documentation errors in the root `CLAUDE.md`:
1. `general:14` -> `general:15` (remote_cache_ttl_secs added in Phase 8)
2. `watch:7` -> `watch:8` (delete_local_after_upload field exists)
3. Remove `named_collection_size` phantom reference from Phase 6 API parity bullet
4. Fix Phase 6 post_actions note -- it claims "dispatches commands" but it was a stub; after this plan it will dispatch correctly. Update to reflect the fix.

**TDD Steps:**
1. Read CLAUDE.md line 55 and line 174.
2. Edit line 55: change `general:14` to `general:15` and `watch:7` to `watch:8`.
3. Edit line 174: remove `named_collection_size`, fix post_actions description.
4. Verify the file is valid markdown.

**Files:** `CLAUDE.md`
**Acceptance:** F006

---

### Task 7: Fix docs/design.md section 7.1 errors

**Goal:** Correct two errors in docs/design.md:
1. Line 983: `"parts_to_do": 3` should be `"parts_to_do": ["all_0_0_0", "all_1_1_0", "all_2_2_0"]` (Vec<String>, not integer)
2. Line 1773: Remove `required_backups` field reference -- the implementation does not use a `required_backups` manifest field; instead it scans `PartInfo.source` for `"carried:{base}"` prefixes.

**TDD Steps:**
1. Read docs/design.md around lines 980-985 and line 1773.
2. Fix parts_to_do example from integer to array of strings.
3. Fix required_backups reference to describe the actual `carried:` source scanning implementation.
4. Verify the file is valid markdown.

**Files:** `docs/design.md`
**Acceptance:** F007

---

### Task 8: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** `src/server` (from affected-modules.json)

**TDD Steps:**

1. Read `src/server/CLAUDE.md` current content.
2. Regenerate directory tree for `src/server/`.
3. Update documentation to reflect:
   - `post_actions` now dispatches actual commands (not a stub)
   - `ListParams` now has `offset`, `limit`, `format` fields
   - `list_backups()` return type includes headers (X-Total-Count)
   - SIGTERM handler added for graceful shutdown
   - `BackupSummary` enrichment wired through `summary_to_list_response()`
4. Validate required sections exist (Parent Context, Directory Structure, Key Patterns, Parent Rules).

**Files:** `src/server/CLAUDE.md`
**Acceptance:** FDOC

---

## Notes

### Phase 4.5 Skip Justification

Phase 4.5 (Interface Skeleton Simulation) is SKIPPED because:
- This plan creates NO new public types, traits, or modules.
- All changes are modifications to existing structs (adding fields) and existing functions (changing bodies/signatures).
- All imports already exist in the files being modified.
- The knowledge_graph.json verified all symbols exist at their stated locations.

### Anti-Overengineering

- `compute_object_disk_size()` sums `s3_objects[].size` directly rather than cross-referencing `disk_types` map. The `s3_objects` field is only populated for S3 disk parts, making the simpler approach correct.
- `extract_required_backup()` returns the first carried base, not a comma-separated list of all bases. A single incremental backup can only have one base (diff-from).
- `format` param in ListParams is stored but not actively used for response formatting (API always returns JSON via axum). This matches Go behavior where the format param exists for integration table DDL compatibility.

### Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (cross-task types) | PASS | Task 3 adds fields to BackupSummary; Task 4 reads them. Same field names and types. |
| RC-016 (struct completeness) | PASS | BackupSummary gets `object_disk_size: u64` and `required: String`. Task 4 uses `s.object_disk_size` and `s.required`. |
| RC-017 (acceptance IDs match tasks) | PASS | F001-F007 + FDOC all referenced in tasks and acceptance.json |
| RC-018 (dependencies satisfied) | PASS | Task 4 depends on Task 3 (same group, sequential). All other tasks are independent. |
| RC-008 (TDD sequencing) | PASS | Task 4 uses `object_disk_size` and `required` on BackupSummary which are added in preceding Task 3. |
| RC-006 (verified APIs) | PASS | All functions referenced (backup::create, upload::upload, etc.) verified via reading existing handler code in routes.rs. |
| RC-019 (existing pattern) | PASS | post_actions dispatch follows existing handler patterns. Pagination follows tables() pattern. Signal handler follows SIGHUP/SIGQUIT pattern. |
