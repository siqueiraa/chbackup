# MR Review: 2026-02-24-01-fix-audit-p1-p2-findings

**Branch:** `claude/2026-02-24-01-fix-audit-p1-p2-findings`
**Base:** `master`
**Commits:** 2 (e6620807, 8fb3f058)
**Files Changed:** 5 (src/cli.rs, src/list.rs, src/main.rs, src/backup/mod.rs, docs/design.md)
**Lines:** +289 / -61

---

## Phase 1: Automated Verification (12 Checks)

### 1. Compilation
- **Status:** **PASS**
- `cargo check` completes with zero errors, zero warnings

### 2. Tests
- **Status:** **PASS**
- 576 tests total: 565 lib + 3 binary + 6 integration + 2 doctests
- 0 failures, 3 ignored (pre-existing)

### 3. Clippy
- **Status:** **PASS**
- Zero clippy warnings

### 4. Formatting
- **Status:** **PASS**
- `cargo fmt --check` reports no formatting issues

### 5. Debug Markers
- **Status:** **PASS**
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` patterns in `src/`

### 6. Unused Code
- **Status:** **PASS**
- Removed `command_name()` function is not referenced anywhere
- No dead code introduced by changes

### 7. Error Handling
- **Status:** **PASS**
- All new error paths use `anyhow::bail!()` or `anyhow::Result` with `.context()`
- `acquire_backup_lock` and `acquire_global_lock` propagate errors via `?`
- Backup collision check provides clear error message with backup name and path

### 8. Acceptance Criteria
- **Status:** **PASS**
- F001 (restore flag conflict): `conflicts_with = "data_only"` on schema field in cli.rs:148
- F002 (shortcut sort): `sort_by(|a, b| a.timestamp.cmp(&b.timestamp))` in list.rs:315
- F003 (lock after resolution): 9 lock call sites, all after shortcut resolution
- F004 (collision detection): `exists()` check + `create_dir` (not `create_dir_all`) in backup/mod.rs:295
- F005 (design doc): `--resume` row updated with deferred note
- F006 (doctests): 2 doctests passing

### 9. Plan Conformance
- **Status:** **PASS**
- All 6 tasks implemented as specified in PLAN.md
- No scope creep, no unplanned changes

### 10. Commit Messages
- **Status:** **PASS**
- `fix: P2 correctness fixes - restore flag conflict, shortcut sort, design doc`
- `fix: P1 correctness fixes - lock after shortcut resolution, backup collision detection`
- Follows conventional commit format, no AI references

### 11. Documentation Consistency
- **Status:** **PASS**
- Design doc updated for `--resume` deferred note
- Code comments explain lock restructuring rationale

### 12. Backward Compatibility
- **Status:** **PASS**
- No API changes, no manifest format changes
- Lock behavior preserves existing semantics (backup-scoped for data commands, global for clean/delete)
- `create_dir_all` for parent still ensures first-time setup works

---

## Phase 2: Design Review (6 Areas)

### 1. Architecture Alignment
- **Status:** **PASS**
- Lock-after-resolution pattern correctly addresses P1-A finding
- `acquire_backup_lock()` and `acquire_global_lock()` are clean helper functions that reduce duplication
- Removed `command_name()` function was only used for lock acquisition -- no other callers

### 2. Correctness
- **Status:** **PASS**
- **P2-A (restore flags):** `conflicts_with = "data_only"` correctly prevents silent no-op when both flags are passed. Clap handles this at parse time. The `conflicts_with` target `"data_only"` correctly references the Rust field name (not CLI flag name `--data-only`).
- **P2-C (shortcut sort):** `Option<DateTime<Utc>>::cmp` sorts `None < Some(_)`, so `None` timestamps go to the front (oldest position). `valid.last()` returns the most recent. This is correct.
- **P1-A (lock bypass):** All 9 command branches that need locks now acquire them AFTER shortcut resolution. The `validate_backup_name` check at line 128 correctly remains BEFORE the match, validating the raw CLI input for path traversal safety before any processing.
- **P1-B (collision):** The `exists()` + `create_dir()` pattern provides defense-in-depth. The TOCTOU gap is acceptable because PidLock serializes same-name operations (after P1-A fix). The `create_dir` call will itself fail atomically if the directory appears between check and create.

### 3. Test Coverage
- **Status:** **PASS**
- `test_restore_schema_and_data_only_conflict`: Verifies clap rejects both flags
- `test_restore_schema_alone_ok` / `test_restore_data_only_alone_ok`: Confirms each flag works alone
- `test_resolve_backup_shortcut_sorts_by_timestamp`: Verifies correct sort order with out-of-order timestamps
- `test_resolve_backup_shortcut_none_timestamps_sort_first`: Verifies None sorts before Some
- `test_create_backup_dir_rejects_existing`: Verifies collision detection
- `test_create_backup_dir_succeeds_when_new`: Verifies happy path still works

### 4. Edge Cases
- **Status:** **PASS**
- Lock on commands with no backup name (list, tables): correctly no lock acquired
- Lock on global commands (clean, clean_broken): correctly uses `acquire_global_lock`
- Lock on delete: correctly after shortcut resolution for both local and remote paths
- Backup collision with empty timestamp (None): correctly sorted to beginning (treated as oldest)
- First backup (parent dir does not exist): `create_dir_all` on parent handles this

### 5. Performance Impact
- **Status:** **PASS**
- No performance changes: lock acquisition is still O(1) filesystem operation
- Shortcut resolution for local is a directory scan (unchanged behavior)
- Shortcut resolution for remote is an S3 list (unchanged behavior, already existed)
- The added timestamp sort in `resolve_backup_shortcut` is O(n log n) but n is the number of backups (typically < 100), negligible

### 6. Security
- **Status:** **PASS**
- Path traversal validation (`validate_backup_name`) remains before any processing (line 128)
- Lock files use resolved names, preventing lock file injection via crafted shortcut names
- No new user input handling without validation

---

## Verdict

**PASS**

All 18 checks pass. The changes correctly address all 5 P1/P2 audit findings with minimal, targeted modifications. Test coverage is comprehensive for the new logic. No regressions detected.

---

## Summary of Changes

| Finding | Severity | Fix | Verification |
|---------|----------|-----|-------------|
| P2-A: restore --schema/--data-only | P2 | `conflicts_with` clap attribute | 3 unit tests |
| P2-C: shortcut sort | P2 | Explicit timestamp sort | 2 unit tests |
| P2-B: design doc --resume | P2 | Doc update with deferred note | Structural grep |
| P1-A: lock bypass on shortcuts | P1 | Lock moved after resolution | 9 call sites verified |
| P1-B: backup name collision | P1 | exists() + create_dir guard | 2 unit tests |
| P2-D: doctests | P2 | Verified passing (no code change) | 2 doctests |
