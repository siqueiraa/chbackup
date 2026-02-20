# MR Review: Phase 0 - Skeleton

**Branch:** `feat/phase0-skeleton`
**Base:** `master`
**Reviewer:** execute-reviewer (Claude)
**Date:** 2026-02-16
**Verdict:** **PASS**

---

## Phase 1: Automated Verification Checks

### Check 1: Compilation
- **Status:** PASS
- `cargo build` completes with zero warnings
- `cargo clippy -- -D warnings` passes clean

### Check 2: Tests
- **Status:** PASS
- 14 tests total: 9 unit tests (lib) + 5 integration tests (config_test.rs)
- All pass: `test result: ok. 14 passed; 0 failed`

### Check 3: Debug Markers
- **Status:** PASS
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` patterns found in `src/`

### Check 4: Debug Prints
- **Status:** PASS
- No `println!`, `dbg!`, or `todo!` found in production code (`src/`)
- All logging uses `tracing::info!` as expected

### Check 5: Unwrap Safety
- **Status:** PASS
- Zero `unwrap()` calls in production code
- All `unwrap()` calls (17 instances) are confined to `#[cfg(test)]` modules only

### Check 6: Blocking-in-Async
- **Status:** PASS
- No `block_on`, `block_in_place`, or `std::thread::sleep` in source code
- Lock file I/O in `lock.rs` uses `std::fs` synchronously, which is acceptable for tiny JSON lock files (< 1KB). Not a blocking concern.

### Check 7: Hardcoded Secrets
- **Status:** PASS
- No hardcoded passwords, API keys, or secrets in source code
- Passwords default to empty string `""`, read from config/env at runtime

### Check 8: Error Handling
- **Status:** PASS
- All user-facing errors use `anyhow::Result` with `.context()` for descriptive messages
- Config parsing errors include file path context
- Lock errors include PID/command/timestamp details
- CLI `--env` parsing returns descriptive error on malformed `KEY=VALUE`
- Unknown config keys return `"Unknown config key: '...'"` error

### Check 9: CLI Subcommands (15 required)
- **Status:** PASS
- All 15 subcommands verified in `--help` output:
  1. create, 2. upload, 3. download, 4. restore
  5. create_remote, 6. restore_remote, 7. list, 8. tables
  9. delete, 10. clean, 11. clean_broken, 12. default-config
  13. print-config, 14. watch, 15. server

### Check 10: CLI Flag Accuracy vs Design Doc
- **Status:** PASS
- `create`: Has `--tables`, `--partitions`, `--diff-from`, `--skip-projections`, `--schema`, `--rbac`, `--configs`, `--named-collections`, `--skip-check-parts-columns`, `--resume`, `[BACKUP_NAME]`
- `upload`: Has `--delete-local`, `--diff-from-remote`, `--resume`, `[BACKUP_NAME]`
- `download`: Has `--hardlink-exists-files`, `--resume`, `[BACKUP_NAME]`
- `restore`: Has `--tables`, `--as`, `-m/--database-mapping`, `--partitions`, `--schema`, `--data-only`, `--rm/--drop`, `--resume`, `--rbac`, `--configs`, `--named-collections`, `--skip-empty-tables`, `[BACKUP_NAME]`
- `create_remote`: Correctly OMITS `--diff-from`, `--partitions`, `--schema` (create-only flags). Has `--diff-from-remote`, `--delete-source`, `--skip-projections`, `--skip-check-parts-columns`.
- `restore_remote`: Correctly OMITS `--partitions`, `--schema`, `--data-only` (restore-only flags). Has `--as`, `-m/--database-mapping`.

### Check 11: Config Param Count
- **Status:** PASS
- 7 config sections present: general, clickhouse, s3, backup, retention, watch, api
- `config.example.yml` has 271 lines with all params documented
- `set_field()` match arms cover all dot-notation keys across all sections

### Check 12: Default Config Output
- **Status:** PASS
- `cargo run -- default-config` outputs valid YAML with all 7 sections
- Output is parseable back to `Config` struct (verified by `test_default_config_serializes`)

---

## Phase 2: Design Review

### Area 1: Architecture & Module Structure
- **Status:** PASS
- Clean module hierarchy: `cli.rs`, `config.rs`, `error.rs`, `lock.rs`, `logging.rs`, `clickhouse/`, `storage/`
- `lib.rs` exposes public modules; `main.rs` is thin orchestration layer
- `cli.rs` is `mod cli` in main.rs (binary-only), not exported from lib -- correct since CLI parsing is binary concern

### Area 2: Config Design
- **Status:** PASS (minor note)
- Priority order correct: YAML file < env vars < CLI `--env` overrides
- Missing config file handled gracefully (falls back to defaults)
- Validation covers: concurrency > 0, watch interval ordering, log level/format values, compression values, rbac_resolve_conflicts values
- **Minor note:** `set_field()` is a large match statement (~120 arms). In future phases, a macro or serde-based approach could reduce boilerplate. Acceptable for Phase 0 skeleton.

### Area 3: Lock Implementation
- **Status:** PASS
- Three-tier scope correctly implemented: Backup(name), Global, None
- Command-to-scope mapping matches design doc section 2
- Stale PID detection via `kill(pid, 0)` on Unix, safe fallback on non-Unix
- Lock file cleaned up via `Drop` trait
- Lock file uses `/tmp/chbackup.{name}.pid` pattern as specified

### Area 4: External Client Wrappers
- **Status:** PASS
- `ChClient`: Uses HTTP URL construction (scheme://host:port), sets credentials conditionally, exposes `ping()` via `SELECT 1`
- `S3Client`: Builds AWS SDK config with region/endpoint/credentials/force_path_style, exposes `ping()` via `ListObjectsV2(max_keys=1)`
- Both are thin wrappers appropriate for Phase 0

### Area 5: Logging
- **Status:** PASS
- JSON mode triggered by `log_format == "json"` OR server command (matches design doc section 11.4)
- `RUST_LOG` env var overrides config log level
- Text mode uses ANSI colors

### Area 6: Commit Quality
- **Status:** PASS
- 9 clean conventional commits, well-scoped:
  - `feat:` prefix for feature additions
  - `feat(cli):`, `feat(config):`, `feat(storage):`, `feat(clickhouse):` scoped prefixes
  - Each commit builds on previous without breaking compilation
- No AI/Claude mentions in commit messages

---

## Issues Summary

### Critical: 0
### Important: 0
### Minor: 2

1. **[Minor]** `src/config.rs:914-1036` -- The `set_field()` method is a ~120-arm match statement mapping dot-notation keys to struct fields. This is functional but verbose. Future phases could benefit from a macro or reflection-based approach. Not blocking for Phase 0.

2. **[Minor]** `src/logging.rs:35` -- `with_ansi(true)` is hardcoded. In a non-TTY context (piped output), ANSI codes may pollute output. The `tracing_subscriber` default behavior auto-detects TTY, so explicitly setting `true` overrides this detection. Low impact since most CLI usage is interactive, and JSON mode (server) does not use ANSI.

---

## Verdict: **PASS**

All 12 automated checks pass. All 6 design review areas pass. Zero critical or important issues found. Two minor observations noted for future improvement. The Phase 0 skeleton is well-structured, compiles cleanly, has comprehensive test coverage for its scope, and faithfully implements the design doc specifications.
