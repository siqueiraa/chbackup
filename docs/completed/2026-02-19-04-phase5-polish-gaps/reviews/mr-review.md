# MR Review: Phase 5 Polish Gaps

**Plan:** 2026-02-19-04-phase5-polish-gaps
**Branch:** feat/phase5-polish-gaps
**Base:** master
**Reviewer:** Claude (manual review)
**Date:** 2026-02-19
**Verdict:** **PASS**

---

## Summary

Phase 5 implements 9 commits covering polish/gap features: API tables and restart endpoints, --skip-projections flag, --hardlink-exists-files download dedup, progress bars via indicatif, structured exit codes, and metadata_size in list API response. All commits follow existing codebase patterns, compile with zero warnings, and all 459 unit tests pass.

---

## Phase 1: Automated Verification Checks

### Check 1: Compilation
- **Status:** PASS
- `cargo check` succeeds with zero warnings

### Check 2: Unit Tests
- **Status:** PASS
- 459 tests pass, 0 failures, 0 ignored

### Check 3: Debug Markers
- **Status:** PASS
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` patterns in `src/`

### Check 4: TODOs/FIXMEs
- **Status:** PASS (with note)
- Two TODOs in `src/server/routes.rs:309-310` are pre-existing (replaced a less-specific TODO), documented as known limitations in CLAUDE.md

### Check 5: Unused Imports/Dependencies
- **Status:** PASS
- New dependencies (indicatif, arc-swap) are all used
- No unused imports detected

### Check 6: Git History
- **Status:** PASS
- 9 commits, all use conventional commit format
- No mention of AI tools in commit messages

### Check 7: No Leftover Stubs
- **Status:** PASS
- `restart_stub` and `tables_stub` endpoints replaced with full implementations
- "not yet implemented" warnings removed from main.rs for `--skip-projections` and `--hardlink-exists-files`

### Check 8: Documentation Updated
- **Status:** PASS
- Root CLAUDE.md updated with Phase 5 status, new patterns, remaining limitations
- src/backup/CLAUDE.md, src/download/CLAUDE.md, src/upload/CLAUDE.md, src/server/CLAUDE.md all updated

### Check 9: New File Registration
- **Status:** PASS
- `src/progress.rs` properly declared in `src/lib.rs` as `pub mod progress`

### Check 10: Error Handling Consistency
- **Status:** PASS
- New code uses `anyhow::Result` with `.context()` / `.with_context()` throughout
- Restart endpoint returns proper HTTP status codes on each failure type
- Hardlink dedup failures gracefully fall back to download

### Check 11: Feature Flag Compatibility
- **Status:** PASS
- No new feature flags introduced
- All changes compile without any feature flags

### Check 12: Config Field Registration
- **Status:** PASS
- `disable_progress_bar` field exists in config.rs (line 47) with default `false` and env overlay support
- `skip_projections` field exists in config.rs (line 370) with default empty vec

---

## Phase 2: Design Review

### Area 1: Architecture and Patterns

**Rating:** Good

- ArcSwap usage in AppState is clean and follows the library's intended pattern
- `.load()` returns a `Guard` that keeps the old value alive for the duration of the handler
- `ProgressTracker` wraps indicatif with proper TTY detection and disabled mode
- Skip projections uses WalkDir's `skip_current_dir()` for efficient pruning
- Hardlink dedup correctly uses `spawn_blocking` for filesystem scanning

### Area 2: Correctness

**Rating:** Good with minor notes

- **Exit code mapping (error.rs):** The `exit_code()` method on `ChBackupError` uses string matching (`msg.contains("not found")`) for the "backup not found" case (code 3). This is fragile -- if error message wording changes, the exit code would regress to 1. However, this is consistent with the design doc's approach and acceptable for now.
- **Restart atomicity (routes.rs:1303-1306):** The three `store()` calls for config/ch/s3 are individually atomic but not collectively atomic. A concurrent handler could momentarily see new config with old s3 client. In practice this is harmless because: (a) handlers load all three at the start of their execution, (b) the old references remain valid, (c) restart is a rare admin operation. Acceptable.
- **Hardlink dedup CRC matching:** Correctly skips parts with CRC64 == 0 (no checksum available). The `find_existing_part` function properly excludes the current backup name.
- **Skip projections:** Correctly checks `.proj` suffix and uses glob matching on the stem. The `*` special case is handled.

### Area 3: Security

**Rating:** Good

- Restart endpoint re-validates config via `config.validate()` before applying
- ClickHouse ping verifies connectivity before swapping clients
- No new credential handling or secret exposure paths
- Tables endpoint with `?backup=` parameter downloads manifest from S3 (no path traversal risk since S3 keys are constructed from backup name)

### Area 4: Performance

**Rating:** Good with documented limitation

- `find_existing_part` scans all local backup directories for each downloaded part (O(backups * parts)). This is documented as acceptable because directory listing is fast and typically few backups exist locally.
- `ProgressTracker` wraps `Arc<ProgressBar>` -- `Clone` and `inc()` are cheap
- Tables endpoint has no pagination (documented limitation)

### Area 5: Test Coverage

**Rating:** Good

- New tests for: hardlink dedup (5 tests), skip projections (3 tests), exit codes (7 tests), progress tracker (4 tests), tables/restart response serialization (5 tests), BackupSummary metadata_size (2 tests)
- All edge cases covered: zero CRC, current backup exclusion, empty pattern list, glob matching, disabled tracker

### Area 6: Consistency with Codebase Conventions

**Rating:** Excellent

- Follows existing error handling patterns (anyhow + context)
- Uses same URL encoding functions for path consistency
- Server routes follow the established try_start_op pattern for async operations
- Progress bar disabled in non-TTY (consistent with server mode expectations)
- Config fields follow existing naming conventions (snake_case, default values)

---

## Issues Found

### Critical
None

### Important
None

### Minor

1. **Non-collective atomicity in restart** (routes.rs:1303-1306): Three separate `ArcSwap::store()` calls could be observed in an inconsistent state by a concurrent handler. Practically harmless but could be noted for future improvement (e.g., storing a single `Arc<(Config, ChClient, S3Client)>` tuple).

2. **String-based exit code matching** (error.rs:48-51): `msg.contains("not found")` for code 3 is fragile. A dedicated `BackupNotFound` error variant would be more robust. Acceptable for current scope.

---

## Verdict: **PASS**

All automated checks pass. Code quality is high, patterns are consistent with the existing codebase, and the implementation correctly addresses all Phase 5 requirements. The two minor notes do not warrant blocking the merge.
