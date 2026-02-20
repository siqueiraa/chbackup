# MR Review: 2026-02-20-01-go-parity-gaps

**Branch:** `claude/go-parity-gaps`
**Base:** `master`
**Reviewer:** Claude (fallback -- Codex not available)
**Date:** 2026-02-20
**Verdict:** **PASS**

---

## Phase 1: Automated Verification Checks (12/12)

### Check 1: Compilation
- **Status:** PASS
- `cargo check` completes with zero errors, zero warnings

### Check 2: Test Suite
- **Status:** PASS
- 479 lib tests + 6 integration tests pass (485 total, 0 failed)

### Check 3: Debug Markers
- **Status:** PASS
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` patterns found in `src/`

### Check 4: Acceptance Criteria
- **Status:** PASS (6/6)
- F001: Config defaults match design doc (timeout 5m, max_connections 1, replica_path without {cluster}, acl empty, check_parts_columns false)
- F002: ch_port defaults to 8123 (HTTP protocol)
- F003: 54 env var reads in `apply_env_overlay()` (up from 18)
- F004: `put_object_with_retry()` and `upload_part_with_retry()` exist and are wired into upload pipeline
- F005: Design doc updated (skip_tables, API 423, backup cleanup, incremental chain protection, multipart CopyObject, ch_port 8123)
- FDOC: All CLAUDE.md files updated (Phase 7 status, server 423, storage retry docs)

### Check 5: No Unrelated Changes
- **Status:** PASS
- All 11 changed files directly relate to the 6 plan tasks

### Check 6: Commit Messages
- **Status:** PASS
- All 7 commits follow conventional commit format (`fix:`, `feat:`, `docs:`, `chore:`)

### Check 7: No TODO/FIXME/HACK Introduced
- **Status:** PASS
- No new TODO/FIXME/HACK markers in the diff

### Check 8: No Secrets/Credentials
- **Status:** PASS
- No secrets, API keys, or credentials in any changed file

### Check 9: Test Coverage for New Code
- **Status:** PASS
- `test_config_defaults_match_design_doc` -- verifies all 5 reverted defaults
- `test_ch_port_default_http` -- verifies port 8123
- `test_env_overlay_coverage` -- verifies all 36 new env vars
- `test_put_object_retry_config` -- verifies PutObject retry wiring
- `test_upload_part_retry_config` -- verifies UploadPart retry wiring
- Existing tests updated: `test_default_config_serializes`, `effective_max_connections` test, `client.rs` test

### Check 10: No Dead Code
- **Status:** PASS
- All new public methods (`put_object_with_retry`, `upload_part_with_retry`, `RetryConfig`) are used in the upload pipeline
- All new env var overlay code is exercised by tests

### Check 11: Error Handling
- **Status:** PASS
- Retry methods propagate final error with `.with_context()` including attempt count and full S3 key
- Env var parse failures silently skip (correct -- invalid env var should not crash startup)

### Check 12: Documentation Consistency
- **Status:** PASS
- Root CLAUDE.md, src/server/CLAUDE.md, src/storage/CLAUDE.md all updated
- Design doc updated at sections 3.4, 3.6, 8.2, 9, 12

---

## Phase 2: Design Review (6 areas)

### Area 1: Architecture Conformance
- **Status:** PASS
- Config default reverts align with design doc section 12
- PutObject/UploadPart retry follows the established `copy_object_with_retry_jitter()` pattern
- Env var overlay follows the existing `apply_env_overlay()` pattern
- No new architectural patterns introduced

### Area 2: Error Handling Quality
- **Status:** PASS
- `put_object_with_retry`: Clones body for each retry attempt (correct -- `put_object` consumes the Vec), logs warning with structured fields (key, attempt, max_retries, delay_ms, error), returns final error with context including full S3 key
- `upload_part_with_retry`: Same pattern with additional upload_id and part_number in log
- Both use `unreachable!()` after the loop -- correct since the loop always returns in the last iteration
- Env var parsing: graceful degradation (silent skip on parse failure) -- consistent with existing pattern

### Area 3: Concurrency Safety
- **Status:** PASS
- `RetryConfig` is `Copy` (all fields are primitive types: u32, u64, f64), safely copied into tokio::spawn closures without explicit Clone
- Body cloning in retry loop is correct -- `put_object()` takes `Vec<u8>` by value
- No shared mutable state introduced

### Area 4: Performance Impact
- **Status:** PASS
- Happy path: zero overhead (retry methods call underlying method directly, return on first success)
- Failure path: exponential backoff with jitter prevents thundering herd
- Body clone on retry: acceptable since retries are rare and the data is already in memory
- Note: `2u64.pow(attempt)` with default max_retries of 3-5 produces delays of 1s-32s, reasonable for transient S3 errors

### Area 5: API/Interface Changes
- **Status:** PASS (non-breaking)
- New `RetryConfig` struct is additive (public, in storage module)
- `put_object_with_retry` and `upload_part_with_retry` are new methods (additive)
- Upload pipeline now calls retry wrappers instead of direct methods -- behavioral change but transparent to callers of `upload()`
- Config defaults changed: this is intentionally breaking for Phase 6 users who relied on the incorrect defaults, but aligns with design doc

### Area 6: Code Quality
- **Status:** PASS with minor notes
- **Minor note 1:** `std::env::set_var` in tests is deprecated as unsafe in Rust 2024 edition (thread-safety). Not blocking for current edition, but worth noting for future migration. The existing codebase already uses this pattern extensively.
- **Minor note 2:** The `test_env_overlay_coverage` test sets 36 env vars and manually cleans up each one. A RAII guard or `serial_test` crate would be cleaner, but the manual cleanup is correct and follows the existing test pattern.
- **Minor note 3:** `put_object_with_retry` and `upload_part_with_retry` have nearly identical retry loop structure. A generic retry helper could reduce duplication, but the current approach matches the existing `copy_object_with_retry_jitter` style and keeps each method self-contained with appropriate logging fields.

---

## Commit-by-Commit Review

### dd2495e8: fix(config): revert Phase 6 defaults to design doc values
- Changes 5 default functions to match design doc section 12
- `default_ch_timeout`: "30m" -> "5m"
- `default_max_connections`: NumCPU/2 -> 1
- `default_replica_path`: removes `{cluster}` segment
- `default_s3_acl`: "private" -> ""
- `check_parts_columns`: removes `default = "default_true"` serde annotation, sets false in Default impl
- Tests updated: `test_config_defaults_match_design_doc` added, `effective_max_connections` test fixed
- **Verdict:** Clean, well-justified reverts

### 69d1f32d: fix(config): change default ch_port from 9000 to 8123
- Resolves design doc internal contradiction (section 12 vs roadmap)
- HTTP protocol (clickhouse crate) uses port 8123, not native TCP 9000
- Tests updated in `client.rs` and `config_test.rs`
- **Verdict:** Correct fix

### e097c915: feat(config): expand env var overlay to cover 54 config fields
- Adds 36 new env var mappings in `apply_env_overlay()` across all 6 config sections
- Each follows the established pattern: `std::env::var()` -> type-specific parse -> assign
- Comprehensive test with set/verify/cleanup for all 36 new vars
- **Verdict:** Mechanical, correct, well-tested

### e5af1a89: feat(storage): add PutObject/UploadPart retry with exponential backoff
- Adds `RetryConfig` struct (Copy, Debug, Clone)
- Adds `put_object_with_retry()` and `upload_part_with_retry()` to S3Client
- Wires retry into upload pipeline (both single PutObject and multipart UploadPart paths)
- Follows `copy_object_with_retry_jitter()` pattern
- Tests verify error messages and method signatures
- **Verdict:** Well-structured, follows existing patterns

### 13d2e371: docs(design): update design doc for genuine Phase 6 improvements
- Adds backup failure cleanup paragraph to section 3.4
- Adds multipart CopyObject note to section 3.6
- Adds incremental chain protection paragraph to section 8.2
- Changes 409 to 423 in section 9
- Changes port 9000 to 8123 in section 12
- Adds `_temporary_and_external_tables.*` to skip_tables in section 12
- **Verdict:** All changes are genuine improvements, correctly documented

### 86f9298f: docs: update CLAUDE.md files for Phase 7 changes
- Root CLAUDE.md: Phase 6 description cleaned up, Phase 7 added
- src/server/CLAUDE.md: 409 -> 423 in 3 places
- src/storage/CLAUDE.md: RetryConfig type and PutObject/UploadPart retry docs added
- **Verdict:** Complete and accurate documentation

### c11d7794: chore: remove debug markers from list.rs
- Removes DEBUG_MARKER:F001 block from `retention_remote()` in list.rs
- **Verdict:** Correct cleanup

---

## Issues Found

### Critical
None

### Important
None

### Minor
1. **Env var test thread safety** (non-blocking): `std::env::set_var` is deprecated as unsafe in Rust 2024 edition. Current edition is unaffected. Future migration should use `serial_test` or RAII guards.
2. **Retry loop duplication** (non-blocking): Three retry methods (`put_object_with_retry`, `upload_part_with_retry`, `copy_object_with_retry_jitter`) share similar retry loop structure. A generic helper could reduce duplication but would sacrifice per-method log field customization.
3. **Potential overflow with extreme retry counts** (non-blocking): `2u64.pow(attempt)` where `attempt` is `u32` could theoretically overflow with `max_retries > 63`. In practice, config defaults are 3-5 and no reasonable user would set retries to 64+. The resulting astronomically long delay would be the natural consequence.

---

## Summary

This MR makes 6 well-scoped changes across 11 files:

1. **Reverts 5 config defaults** to design doc values (undoing incorrect Phase 6 Go copies)
2. **Fixes ch_port** from 9000 to 8123 (resolving design doc contradiction)
3. **Expands env var overlay** from 18 to 54 fields (fulfilling design doc section 2)
4. **Adds PutObject/UploadPart retry** with exponential backoff and jitter (genuine missing feature)
5. **Updates design doc** for 6 genuine Phase 6 improvements
6. **Updates CLAUDE.md** documentation across 3 files

All 485 tests pass. Zero compilation warnings. Zero debug markers. All 6 acceptance criteria satisfied. Code follows existing patterns throughout. No breaking changes beyond the intentional config default reverts.
