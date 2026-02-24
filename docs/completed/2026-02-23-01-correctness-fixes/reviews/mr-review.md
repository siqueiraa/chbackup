# MR Review: Fix 7 Correctness Issues from Security/Quality Audit

**Verdict: PASS**

**Branch:** `fix/correctness-audit-issues`
**Base:** `master`
**Commits:** 5 (cbf1a3d2, 9ee9b616, 17b6f855, a1c8be25, cedd2681)
**Files changed:** 16 (797 insertions, 256 deletions)
**Reviewer:** Claude (Codex unavailable)
**Date:** 2026-02-23

---

## Phase 1: Automated Verification (12 checks)

### Check 1: Compilation
- **Status:** PASS
- `cargo check` completes with zero errors
- Zero compiler warnings

### Check 2: Test Suite
- **Status:** PASS
- 561 tests pass, 3 ignored (async S3 network tests, intentionally marked `#[ignore]`)
- Doc-tests pass (2 tests in path_encoding.rs)
- Integration tests pass (6 tests in config_test.rs)

### Check 3: Debug Markers
- **Status:** PASS
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` markers found in src/

### Check 4: Acceptance Criteria
- **Status:** PASS (8/8)
- F001 (path_encoding module): PASS -- `encode_path_component` and `sanitize_path_component` exported, 9 unit tests
- F002 (disable_cert_verification HTTP fallback): PASS -- Broken `AWS_CA_BUNDLE` env var removed, HTTP endpoint rewriting implemented, 3 tests
- F003 (hermetic S3 tests): PASS -- `mock_s3_fields` helper avoids TLS init, 3 async tests marked `#[ignore]`
- F004 (disable_ssl wiring): PASS -- `config.disable_ssl` read in `S3Client::new()`, endpoint rewritten, 3 tests
- F005 (check_parts_columns strict-fail): PASS -- `bail!()` replaces `info!("proceeding anyway")`, 3 new tests
- F006 (env-style --env keys): PASS -- `env_key_to_dot_notation()` maps 54+ keys, 5 tests
- F007 (DRY path encoding): PASS -- All 4 `url_encode` functions removed, replaced by canonical module
- FDOC (documentation): PASS -- 5 CLAUDE.md files updated, design.md updated

### Check 5: Zero Warnings
- **Status:** PASS
- `cargo check 2>&1 | grep -ci "warning\["` returns 0

### Check 6: No Dead Code Introduced
- **Status:** PASS (with minor note)
- `sanitize_path_component()` is exported but not called from production code (download/mod.rs uses `Path::components()` filter instead, which is a stronger approach). The function is well-tested and available as public API for future callers. Not blocking.

### Check 7: No Unsafe Code Introduced
- **Status:** PASS
- The only `unsafe` blocks are in test code (`std::env::set_var`/`std::env::remove_var` in config tests), properly annotated with `// SAFETY: single-threaded test` comments. This is the correct approach for Rust 2024 edition where `set_var` is `unsafe`.

### Check 8: No Secrets or Sensitive Data
- **Status:** PASS
- No credentials, API keys, or sensitive data in any committed file

### Check 9: Conventional Commits
- **Status:** PASS
- All 5 commits follow conventional format: `test:`, `fix:`, `refactor:`, `fix:`, `docs:`

### Check 10: No AI References
- **Status:** PASS
- No mentions of Claude, AI, or any AI tool in commit messages or code

### Check 11: Backward Compatibility
- **Status:** PASS
- `encode_path_component` produces identical output to all 4 old implementations for non-adversarial inputs
- `check_parts_columns` strict-fail only triggers when `check_parts_columns=true` (default false) AND unfiltered inconsistencies exist; `--skip-check-parts-columns` override available
- `env_key_to_dot_notation` returns `None` for unrecognized keys, falling through to existing dot-notation handling
- `unsafe` blocks around `set_var`/`remove_var` in tests correctly handle Rust 2024 edition requirements

### Check 12: Test Coverage for New Code
- **Status:** PASS
- path_encoding.rs: 9 unit tests + 2 doc-tests covering basic, special chars, multi-byte UTF-8, slash encoding, sanitization, dot/dotdot rejection
- s3.rs: 6 new tests (3 disable_ssl, 3 disable_cert_verification) + 1 structural test
- config.rs: 5 new tests (3 env_key_to_dot_notation, 2 CLI override integration)
- backup/mod.rs: 3 new tests (strict-fail, benign drift, query error)

---

## Phase 2: Design Review (6 areas)

### Area 1: Correctness
- **Status:** PASS
- **Path encoding consolidation**: The unified `encode_path_component` correctly uses byte-level encoding for multi-byte UTF-8 (matching the most correct of the 4 old implementations). The decision to NOT preserve `/` is correct since all callers pass individual db/table name components.
- **Path traversal prevention**: The download/mod.rs fix using `Path::components()` with `Normal`-only filter is the correct approach -- it operates at the type level (ParentDir variant rejected), not string matching. Properly handles edge cases like empty results.
- **TLS config**: The `disable_cert_verification` -> HTTP fallback is pragmatic given AWS SDK limitations. The `bail!` on empty endpoint prevents silent misconfiguration.
- **check_parts_columns**: The strict-fail with `bail!()` is correct -- the old "proceeding anyway" behavior could lead to restore failures downstream. Query-level errors remain warn-only (correct -- query infra failure should not block backup).
- **env_key_to_dot_notation**: The static match table approach is correct (zero allocations, compile-time exhaustiveness). The fallthrough to dot-notation for unrecognized keys preserves backward compatibility.

### Area 2: Security
- **Status:** PASS
- Path traversal: Fixed in download/mod.rs with `Path::components()` Normal-only filter
- Broken TLS bypass removed: `std::env::set_var("AWS_CA_BUNDLE", "")` was unsafe (global process mutation, race conditions, false sense of security) -- correctly replaced with HTTP endpoint rewriting
- `sanitize_path_component` provides defense-in-depth API for future callers

### Area 3: Performance
- **Status:** PASS
- No performance regressions. All changes are to string processing, config parsing, or error handling code paths.
- `encode_path_component` uses `String::with_capacity(s.len())` for pre-allocation.
- `env_key_to_dot_notation` is a constant-time match statement.

### Area 4: Error Handling
- **Status:** PASS
- New error paths follow existing patterns (anyhow `bail!`, `Context`)
- check_parts_columns: `bail!` for actionable inconsistencies, `warn!` for query errors
- disable_cert_verification: `bail!` for empty endpoint (unrecoverable config error)
- Path traversal: `warn!` + `continue` for unsafe paths (graceful degradation)

### Area 5: Code Quality
- **Status:** PASS
- DRY improvement: 4 duplicated url_encode functions consolidated into 1
- Well-documented: module-level doc comments, function doc comments with examples, inline comments explaining design decisions
- Test quality: Each new function has unit tests covering normal, edge, and error cases
- Unsafe code properly annotated with safety comments

### Area 6: Documentation
- **Status:** PASS
- 5 module CLAUDE.md files updated with new patterns
- design.md updated for `--env` env-style keys and `disable_cert_verification` HTTP semantics
- All behavioral changes documented in PLAN.md with risk assessment

---

## Issues Found

### Critical: None

### Important: None

### Minor

1. **Unused exported function** (path_encoding.rs): `sanitize_path_component()` is exported but not called from production code. The download path traversal fix uses `Path::components()` instead (stronger approach). The function remains useful as public API. **Severity: Minor, Non-blocking.**

2. **Test duplication** (collect.rs, download/mod.rs, upload/mod.rs, attach.rs): Several test files still have module-level tests for `encode_path_component` (e.g., `test_encode_path_component_simple` in collect.rs) that duplicate tests already in path_encoding.rs. These are harmless (extra test coverage) but add minor maintenance burden. **Severity: Minor, Non-blocking.**

---

## Summary

This MR cleanly addresses 7 correctness issues from the security/quality audit:
1. Path traversal prevention via `Path::components()` Normal-only filter in download
2. Broken `AWS_CA_BUNDLE` env var approach removed, replaced with HTTP endpoint fallback
3. S3 unit tests made hermetic with `mock_s3_fields` (no TLS init)
4. `disable_ssl` config wired into S3Client construction
5. `check_parts_columns` now fails with error instead of warning+continue
6. `--env` flag accepts env-style keys (S3_BUCKET=val) via static translation table
7. 4 duplicated `url_encode` functions replaced by canonical `path_encoding` module

All 561 tests pass, zero warnings, zero debug markers, all 8 acceptance criteria met. The changes are well-tested, backward-compatible, and follow existing code patterns.

**PASS**
