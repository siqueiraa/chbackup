# Plan: Remove Dead Code, Unused Fields, and Unnecessary Public APIs

## Goal

Remove 8 confirmed dead code items (fields, methods, and a function) from the chbackup codebase to reduce maintenance surface and eliminate `#[allow(dead_code)]` suppressions, while preserving the zero-warnings compilation baseline.

## Architecture Overview

This plan targets three source modules:
1. **src/clickhouse/client.rs** -- Remove dead `debug` field and unused `inner()` getter
2. **src/storage/s3.rs** -- Remove unused `inner()`, `concurrency()`, `object_disk_path()` getters and the `concurrency`/`object_disk_path` struct fields they read
3. **src/restore/attach.rs** -- Remove dead `attach_parts()` function (superseded by `attach_parts_owned()`)

All items have zero external callers, verified via LSP `findReferences` and grep cross-validation. See `context/references.md` for evidence.

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **ChClient**: Created by `main.rs` and `server/state.rs` (reload/restart), stored in `AppState` via `Arc<ArcSwap<ChClient>>`
- **S3Client**: Created by `main.rs` and `server/state.rs` (reload/restart), stored in `AppState` via `Arc<ArcSwap<S3Client>>`
- **attach_parts()**: Defined in `restore/attach.rs`, called by nobody (dead). `attach_parts_owned()` and `attach_parts_inner()` are the live functions.

### What This Plan CANNOT Do
- Cannot remove `ChClient.inner` field (heavily used internally for queries)
- Cannot remove `S3Client.inner` field (heavily used internally for S3 operations)
- Cannot remove `AttachParams` struct (used by `attach_parts_owned()` as bridge to `attach_parts_inner()`)
- Cannot remove `attach_parts_inner()` (called by `attach_parts_owned()`)
- Cannot remove Prometheus counters `parts_uploaded_total`/`parts_skipped_incremental_total` (convention: metrics should exist at zero)
- Cannot remove `ChBackupError` variants (error taxonomy for exit code mapping)
- Cannot remove test-only helpers (`ProgressTracker::disabled`, `ProgressTracker::is_active`, `ActionLog::running`)
- Cannot remove any Arc/Mutex/ArcSwap wrapping in AppState (all architecturally required)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Removing a field/method that is actually used | GREEN | All 8 items verified with LSP findReferences returning 0 external callers |
| Breaking test compilation after field removal | GREEN | Only S3Client test helper needs update (remove 2 field initializations) |
| Breaking zero-warnings policy | GREEN | Removing `#[allow(dead_code)]` annotations actually makes this more robust |
| Config fields diverging from struct fields | GREEN | Config fields (`s3.concurrency`, `s3.object_disk_path`) are NOT removed; only the S3Client struct fields that stored but never read them |

## Expected Runtime Logs

This is a pure dead-code removal plan. No runtime behavior changes.

| Pattern | Required | Description |
|---------|----------|-------------|
| No new log patterns | N/A | Pure deletion, no new log lines |
| `ERROR:` | no (forbidden) | Should NOT appear due to these changes |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Unused Prometheus counters (parts_uploaded_total, parts_skipped_incremental_total) | Convention: metrics should exist at zero values | Wire `.inc()` calls when upload instrumentation is added |
| Unused ChBackupError variants (ClickHouseError, S3Error, ConfigError, BackupError, RestoreError, ManifestError) | Error taxonomy for exit code mapping | Construct from production error paths when needed |
| Test-only public methods (disabled(), is_active(), running()) | Intentional test helpers used in #[cfg(test)] | Keep as-is |
| `pub` -> `pub(crate)` visibility suggestions (cli.rs) | Cosmetic; not dead code | Address in a separate visibility audit |
| Config fields `s3.concurrency` and `s3.object_disk_path` still parsed from YAML | Forward compatibility; config parsing is cheap | Wire to actual usage or remove in a future config audit |

## Dependency Groups

```
Group A (Independent -- all 3 tasks can execute in parallel):
  - Task 1: Remove dead code from ChClient (src/clickhouse/client.rs)
  - Task 2: Remove dead code from S3Client (src/storage/s3.rs)
  - Task 3: Remove dead attach_parts() function (src/restore/attach.rs)

Group B (Sequential -- after Group A):
  - Task 4: Update CLAUDE.md for all modified modules (MANDATORY)
```

## Tasks

### Task 1: Remove dead code from ChClient

**Description:** Remove the `debug` field (with its `#[allow(dead_code)]` annotation) and the `inner()` getter method from `ChClient`.

**TDD Steps:**
1. Run `cargo check` to confirm clean baseline
2. Remove the `#[allow(dead_code)]` annotation (line 23) and `debug: bool` field (line 24) from the `ChClient` struct
3. Remove the `debug: config.debug` assignment from the constructor (line 220)
4. Remove the `inner()` method (lines 248-254, including doc comment)
5. Run `cargo check` -- must compile with zero errors and zero warnings
6. Run `cargo test -p chbackup` -- all tests must pass
7. Run `cargo fmt -- --check` -- no formatting issues

**Files:** `src/clickhouse/client.rs`
**Acceptance:** F001

**Implementation Notes:**
- The `config.debug` is still READ at line 211 for the conditional info log and at line 219 for the `log_sql_queries` override. Only the STORED `debug` field is removed.
- The `inner` FIELD is NOT removed -- only the `pub fn inner()` GETTER is removed. The field is used internally by 10+ methods.
- After removing the field, the `/// Stored for future use; currently wired via log_sql_queries override.` doc comment on the line above should also be removed.

---

### Task 2: Remove dead code from S3Client

**Description:** Remove the `inner()`, `concurrency()`, and `object_disk_path()` getter methods, and the `concurrency` and `object_disk_path` struct fields from `S3Client`. Update the test helper that constructs `S3Client` directly.

**TDD Steps:**
1. Run `cargo check` to confirm clean baseline
2. Remove the `concurrency: u32` field (line 58) and `object_disk_path: String` field (line 60) from the `S3Client` struct, including their doc comments (lines 57, 59)
3. Remove the `concurrency: config.concurrency` assignment (line 187) and `object_disk_path: config.object_disk_path.clone()` assignment (line 188) from `S3Client::new()`
4. Remove the `pub fn inner()` method (lines 219-222, including doc comment line 219)
5. Remove the `pub fn concurrency()` method (lines 234-237, including doc comment line 234)
6. Remove the `pub fn object_disk_path()` method (lines 239-245, including doc comments lines 239-242)
7. Update the test helper `mock_s3_client()` (around line 1558-1568): remove the `concurrency: 1` and `object_disk_path: String::new()` field initializations from the struct literal
8. Run `cargo check` -- must compile with zero errors and zero warnings
9. Run `cargo test -p chbackup` -- all tests must pass
10. Run `cargo fmt -- --check` -- no formatting issues

**Files:** `src/storage/s3.rs`
**Acceptance:** F002

**Implementation Notes:**
- The `inner` FIELD is NOT removed -- only the `pub fn inner()` GETTER is removed. The field is used internally by 7+ methods for actual S3 operations.
- The `bucket()` and `prefix()` getters are NOT removed -- they have callers.
- The config fields `s3.concurrency` and `s3.object_disk_path` in `src/config.rs` are NOT removed. They are still parsed from YAML but simply not stored on the S3Client struct anymore.

---

### Task 3: Remove dead attach_parts() function

**Description:** Remove the dead `attach_parts()` function and its `#[allow(dead_code)]` annotation from `src/restore/attach.rs`. The function was superseded by `attach_parts_owned()` which is the sole production caller of `attach_parts_inner()`.

**TDD Steps:**
1. Run `cargo check` to confirm clean baseline
2. Remove the `#[allow(dead_code)]` annotation (line 486) and the entire `attach_parts()` function (lines 482-491, including doc comment)
3. Run `cargo check` -- must compile with zero errors and zero warnings
4. Run `cargo test -p chbackup` -- all tests must pass
5. Run `cargo fmt -- --check` -- no formatting issues

**Files:** `src/restore/attach.rs`
**Acceptance:** F003

**Implementation Notes:**
- `AttachParams` struct is NOT removed -- it is used internally by `attach_parts_owned()` as a bridge to `attach_parts_inner()`.
- `attach_parts_inner()` is NOT removed -- it is the core logic called by `attach_parts_owned()`.
- Only the public wrapper `attach_parts()` that accepts borrowed `&AttachParams<'_>` is removed.

---

### Task 4: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/clickhouse, src/storage, src/restore

**TDD Steps:**

1. **Update `src/clickhouse/CLAUDE.md`:**
   - Remove `inner() -> &clickhouse::Client` from the Public API section
   - Remove `debug` from the ChClient struct field list description
   - Remove the bullet "Verbose debug logging..." from the Client Wrapper Pattern section if it mentions the `debug` field
   - Keep all other documentation intact

2. **Update `src/storage/CLAUDE.md`:**
   - Remove `inner() -> &aws_sdk_s3::Client` from the Public API section
   - Remove `concurrency() -> u32` from the Public API section (the "S3 concurrency + object_disk_path" entry)
   - Remove `object_disk_path() -> &str` from the Public API section
   - Remove the `concurrency` and `object_disk_path` fields from the S3Client description
   - Keep all other documentation intact

3. **Update `src/restore/CLAUDE.md`:**
   - Remove `attach_parts(params) -> Result<u64>` from the Public API section
   - Update the Part Attachment section to remove mention of the borrowed-params variant
   - Keep `attach_parts_owned(params) -> Result<u64>` and `attach_parts_inner()` documentation
   - Keep all other documentation intact

4. **Validate all CLAUDE.md files:**
   - Each must contain: Parent Context, Directory Structure, Key Patterns, Parent Rules sections

**Files:** `src/clickhouse/CLAUDE.md`, `src/storage/CLAUDE.md`, `src/restore/CLAUDE.md`
**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-015 (Symbols match) | PASS | All 8 dead items verified via LSP/grep in context/references.md |
| RC-016 (Test names match) | PASS | No new tests -- only verifying existing tests still pass |
| RC-017 (Acceptance IDs match tasks) | PASS | F001->Task 1, F002->Task 2, F003->Task 3, FDOC->Task 4 |
| RC-018 (Dependencies satisfied) | PASS | Tasks 1-3 are independent; Task 4 depends on Tasks 1-3 |
| RC-021 (File locations verified) | PASS | All file paths verified via grep in context/symbols.md |
| RC-035 (cargo fmt) | PASS | Each task includes `cargo fmt -- --check` step |

## Notes

**Phase 4.5 (Interface Skeleton Simulation) skipped:** This plan removes code only; no new imports, types, or function signatures are introduced. Compilation is verified by `cargo check` after each removal.
