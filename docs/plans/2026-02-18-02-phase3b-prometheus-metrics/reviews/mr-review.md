# MR Review: Phase 3b Prometheus Metrics

**Branch:** `phase3b-prometheus-metrics`
**Base:** `master`
**Reviewer:** Claude (fallback)
**Date:** 2026-02-18

---

## Verdict: **PASS**

---

## Phase 1: Automated Verification Checks (12/12)

| # | Check | Result | Details |
|---|-------|--------|---------|
| 1 | Compilation | PASS | `cargo check` succeeds with zero errors |
| 2 | Zero warnings | PASS | No compiler warnings |
| 3 | All tests pass | PASS | 273 tests passed, 0 failed |
| 4 | No debug markers | PASS | 0 `DEBUG_MARKER`/`DEBUG_VERIFY` found in `src/` |
| 5 | Conventional commits | PASS | All 7 commits follow conventional format |
| 6 | No AI mentions | PASS | "CLAUDE.md" in commit is the project doc filename, not an AI reference |
| 7 | F001: Metrics struct | PASS | `grep -c 'pub struct Metrics' src/server/metrics.rs` = 1 |
| 8 | F002: metrics handler | PASS | `grep -cE 'pub async fn metrics\(' src/server/routes.rs` = 1 |
| 9 | F003: duration refs | PASS | 21 references to `backup_duration_seconds` in routes.rs |
| 10 | F004: errors_total refs | PASS | 11 references to `errors_total` in routes.rs |
| 11 | FDOC: documentation | PASS | `metrics.rs` mentioned in `src/server/CLAUDE.md` |
| 12 | No unwrap on user data | PASS | Zero `.unwrap()` calls in metrics.rs or routes.rs (production code) |

## Phase 2: Design Review (6 areas)

### 1. Architecture & Design

**PASS** - Clean separation of concerns:
- `metrics.rs` defines the `Metrics` struct with a custom (non-global) `prometheus::Registry` -- excellent for testing isolation
- `state.rs` conditionally creates `Option<Arc<Metrics>>` based on `config.api.enable_metrics`
- `routes.rs` instruments all 9 operation handlers with `if let Some(m)` guards
- `mod.rs` replaces `metrics_stub` with the real `metrics` handler

The custom registry approach is better than using the global prometheus registry -- it prevents metric conflicts in test environments and enables clean unit testing.

### 2. Error Handling

**PASS** - Robust error handling throughout:
- `Metrics::new()` returns `Result<Self, prometheus::Error>` -- no panics on metric registration failure
- `AppState::new()` gracefully degrades if metrics creation fails (logs warning, continues with `None`)
- `/metrics` endpoint handles: disabled metrics (501), encode error (500), success (200)
- All handler instrumentation uses `if let Some(m)` pattern -- never panics if metrics disabled
- `encode()` uses `unwrap_or_default()` for UTF-8 conversion (safe -- TextEncoder always produces valid UTF-8)

### 3. Correctness

**PASS** - All 9 operations instrumented correctly:
- `create`: duration + success + size + timestamp | duration + error
- `upload`: duration + success + timestamp | duration + error
- `download`: duration + success | duration + error
- `restore`: duration + success | duration + error
- `create_remote`: duration + success + size + timestamp (on upload success) | duration + error (on either create or upload failure)
- `restore_remote`: duration + error (on download failure) | duration + success/error (on restore success/failure)
- `delete`: duration + success | duration + error
- `clean_broken_remote`: duration + success | duration + error
- `clean_broken_local`: duration + success | duration + error

Duration is recorded in both success and failure paths, which is correct for monitoring.

The `create_remote` handler correctly captures the `BackupManifest` return value (changed from `if let Err(e)` to `match` with `Ok(manifest)`) to set `backup_size_bytes` and `backup_last_success_timestamp`.

### 4. Performance

**PASS** with documented tradeoff:
- Scrape-time refresh calls `list_local` (via `spawn_blocking`) and `list_remote` (async S3 ListObjects) on every `/metrics` scrape
- This is documented as intentional for 15-30s scrape intervals
- `in_progress` gauge is computed from `current_op` lock state -- lightweight
- Label pre-initialization in `Metrics::new()` ensures all operation labels appear in output even before first observation

Minor note: concurrent Prometheus scrapes could trigger parallel S3 ListObjects calls. This is a documented tradeoff (see SESSION.md) and is acceptable for typical monitoring setups with a single Prometheus scraper.

### 5. Test Coverage

**PASS** - Comprehensive test suite:
- `test_metrics_new_registers_all`: verifies all 14 metric families are registered
- `test_metrics_encode_text`: verifies prometheus text format output
- `test_metrics_counter_increment`: verifies counter increment and re-increment
- `test_metrics_duration_observation`: verifies histogram observation with count and sum
- `test_metrics_error_increment`: verifies error counter
- `test_metrics_success_increment`: verifies success counter
- `test_metrics_size_gauge`: verifies gauge set
- `test_metrics_handler_returns_prometheus_text`: verifies handler success path output
- `test_metrics_handler_disabled`: verifies handler None path returns 501
- `test_app_state_with_metrics_enabled`: verifies AppState creates metrics when enabled
- `test_app_state_with_metrics_disabled`: verifies AppState skips metrics when disabled
- `test_phase3b_prometheus_available`: integration test for prometheus crate types

### 6. Documentation

**PASS** - All documentation updated:
- `src/server/CLAUDE.md` updated with:
  - `metrics.rs` in module file listing
  - `metrics: Option<Arc<Metrics>>` in AppState fields
  - Full "Prometheus Metrics" section with all 14 metric families
  - Scrape-time refresh explanation
  - Operation instrumentation description
  - Public API entries for `Metrics::new()` and `Metrics::encode()`
- `/metrics` removed from stub endpoints list
- `AppState::new()` description updated to mention optional metrics

---

## Commit Quality

7 commits, well-structured in logical progression:

1. `feat(deps): add prometheus 0.13 dependency` -- minimal Cargo.toml change
2. `feat(server): add Metrics struct with 14 prometheus metric definitions` -- new module
3. `feat(server): add metrics field to AppState with conditional creation` -- wiring
4. `feat(server): replace metrics_stub with real /metrics handler` -- endpoint
5. `feat(server): instrument operation handlers with prometheus metrics` -- instrumentation
6. `docs(server): update CLAUDE.md with metrics module documentation` -- docs
7. `chore: remove debug markers for Phase 3b metrics` -- cleanup

Each commit is atomic and builds on the previous one. The progression is logical: dependency -> types -> wiring -> endpoint -> instrumentation -> docs -> cleanup.

---

## Issues Found

### Critical: 0
### Important: 0
### Minor: 0

---

## Summary

This is a clean, well-structured implementation of Phase 3b Prometheus Metrics. The code follows the established patterns in the codebase (custom registry, Option-based conditional metrics, if-let guards in handlers). All 14 metric families from the design doc are registered, all 9 operation handlers are instrumented with duration/success/error tracking, and the `/metrics` endpoint correctly serves Prometheus text exposition format with scrape-time refresh of backup count gauges. Tests are comprehensive and all 273 pass. No issues requiring fixes.
