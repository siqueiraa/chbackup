# MR Review: fix/api-gaps

**Reviewer:** execute-reviewer agent
**Date:** 2026-02-20
**Branch:** fix/api-gaps (8 commits, 8 files changed, +675/-61 lines)
**Verdict:** **PASS**

---

## Phase 1: Automated Verification Checks

### Check 1: Compilation
**Status:** PASS
- `cargo check` succeeds with zero errors.

### Check 2: Tests
**Status:** PASS
- 527 unit tests pass, 0 failures, 0 ignored.
- New tests added: `test_backup_summary_object_disk_size`, `test_compute_object_disk_size_sums_s3_objects`, `test_extract_required_from_manifest`, `test_extract_required_empty_for_full_backup`, `test_list_params_deserialization`.
- Existing test `test_summary_to_list_response_sizes` updated to verify new field wiring.

### Check 3: Clippy
**Status:** PASS
- Zero clippy warnings.

### Check 4: Formatting
**Status:** PASS (fixed in iteration 2)
- Initial review found 5 formatting issues in `src/server/routes.rs`.
- Fixed by running `cargo fmt` and committing (e5b07058).
- `cargo fmt --check` now passes with zero differences.

### Check 5: Debug Markers
**Status:** PASS
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` markers found in `src/`.

### Check 6: Acceptance Criteria
**Status:** PASS (all 8 criteria pass in code, acceptance.json stale for F004/F007)
- F001 (post_actions dispatch): PASS -- `crate::backup::create` appears 3 times in routes.rs.
- F002 (list pagination): PASS -- `ListParams` has `offset`, `limit`, `format`; `x-total-count` header present.
- F003 (BackupSummary fields): PASS -- `object_disk_size: u64` and `required: String` added with `#[serde(default)]`.
- F004 (ListResponse wiring): PASS -- `summary_to_list_response()` uses `s.object_disk_size` and `s.required`.
- F005 (SIGTERM handler): PASS -- `SignalKind::terminate()` registered, `shutdown_signal()` used in both TLS and plain paths.
- F006 (CLAUDE.md fixes): PASS -- `general:15`, `watch:8`, `named_collection_size` removed.
- F007 (design.md fixes): PASS -- `parts_to_do` is array, `required_backups` field reference replaced with `carried:` scanning description.
- FDOC (server CLAUDE.md): PASS -- Updated with post_actions dispatch, list pagination, SIGTERM handler, summary wiring.

### Check 7: Commit Messages
**Status:** PASS
- All 8 commits follow conventional commit format.
- No AI/Claude references in commits.

### Check 8: Branch Hygiene
**Status:** PASS
- Branch is `fix/api-gaps` based on `master`.
- No merge commits; clean linear history.

### Check 9: No Secrets
**Status:** PASS
- No `.env`, credentials, or secret files in the changeset.

### Check 10: Warning-Free
**Status:** PASS
- Zero compiler warnings, zero clippy warnings.

### Check 11: Backward Compatibility
**Status:** PASS
- New `BackupSummary` fields use `#[serde(default)]` for backward-compatible deserialization.
- `ListParams` new fields are all `Option<T>` (existing clients unaffected).
- `list_backups()` return type changed to include headers, but JSON payload unchanged.

### Check 12: Test Coverage
**Status:** PASS
- All new helper functions (`compute_object_disk_size`, `extract_required_backup`) have dedicated unit tests.
- Pagination deserialization tested. Summary-to-response wiring tested with assertions on new fields.
- Signal handler untestable (acknowledged in plan as acceptable).

---

## Phase 2: Design Review

### Area 1: Correctness
**Status:** PASS
- `compute_object_disk_size()` correctly sums `s3_objects[].size` with `saturating_add` (overflow-safe).
- `extract_required_backup()` correctly uses `strip_prefix("carried:")` matching the existing `collect_incremental_bases()` pattern.
- `post_actions` dispatch matches existing handler patterns with correct default parameters.
- `shutdown_signal()` correctly uses `tokio::select!` for SIGINT/SIGTERM on Unix, SIGINT-only on non-Unix.

### Area 2: Error Handling
**Status:** PASS
- `post_actions` dispatch properly calls `finish_op`/`fail_op` in all paths.
- Unknown commands return 400 BAD_REQUEST (both in outer match and inner unreachable branch).
- `delete` command handles `spawn_blocking` join errors.
- `clean_broken` combines S3 and local results, failing if either fails.
- Metrics are recorded for both success and failure paths.

### Area 3: Performance
**Status:** PASS
- `compute_object_disk_size` and `extract_required_backup` are O(N) iterations over manifest data (in-memory, no I/O).
- Pagination uses `skip().take()` on `Vec` (after full collection), which is the same approach as the existing `tables()` endpoint.
- No N+1 query patterns introduced.
- `shutdown_signal()` uses `tokio::select!` which is zero-cost until a signal arrives.

### Area 4: Security
**Status:** PASS
- No new endpoints added; existing auth middleware continues to protect all routes.
- `post_actions` dispatch reuses existing command functions (no privilege escalation).
- No user input used in file paths or SQL queries without existing sanitization.

### Area 5: Concurrency
**Status:** PASS
- `post_actions` dispatch goes through `try_start_op()` which acquires semaphore (same as all other operation handlers).
- Manifest cache invalidation follows existing pattern (lock -> invalidate).
- `shutdown_signal()` is called from a single tokio task (no race).

### Area 6: API Contract
**Status:** PASS
- `ListResponse` gains `object_disk_size` and `required` fields that were already defined in the struct but hardcoded to 0/empty. Now populated from manifest data.
- `X-Total-Count` header added to list endpoint (additive, non-breaking).
- `ListParams` gains optional fields (additive, non-breaking).
- POST /api/v1/actions now dispatches actual commands instead of no-op (bug fix, matches documented behavior).

---

## Issues Found

### Critical

None (formatting issue fixed in iteration 2, commit e5b07058).

### Important

None.

### Minor

1. **Stale acceptance.json** -- F004 and F007 show `"status": "fail"` but the code is correct. The acceptance.json was not updated after these tasks completed. Non-blocking but should be corrected.

---

## Fix History

| Iteration | Issue | Fix | Commit |
|-----------|-------|-----|--------|
| 1 | `cargo fmt --check` fails (5 formatting diffs in routes.rs) | Ran `cargo fmt` | e5b07058 |

---

## Summary

The branch implements all 8 planned tasks correctly. Code compiles, all 527 tests pass, clippy is clean, formatting is clean. One formatting issue was found and fixed during the review (iteration 2). All 18 review checks pass.
