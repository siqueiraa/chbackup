# MR Review: Server API Kill, Concurrency, and Safety Fixes

**Plan:** 2026-02-21-01-server-kill-concurrency-safety
**Branch:** fix/server-kill-concurrency-safety (master..fix/server-kill-concurrency-safety)
**Reviewer:** Claude (execute-reviewer agent)
**Date:** 2026-02-21
**Verdict:** **PASS**

---

## Phase 1: Automated Verification (12 Checks)

### Check 1: Plan Completeness

**Status:** PASS

`acceptance.json` shows 10/10 features at `status: "pass"`:
- F001 (backup name validation) - PASS
- F002 (PID lock TOCTOU fix) - PASS
- F003 (running_ops HashMap) - PASS
- F004 (CancellationToken wiring) - PASS
- F005 (run_operation DRY helper) - PASS
- F006 (reload semantics fix) - PASS
- F007 (upload auto-retention) - PASS
- F008 (create --resume docs) - PASS
- F009 (integration tests T4-T10) - PASS
- FDOC (CLAUDE.md updates) - PASS

### Check 2: Code Quality

**Status:** PASS (with minor formatting note)

- `cargo check`: Clean, zero errors, zero warnings
- `todo!`/`unimplemented!`/`TODO`/`FIXME`: 0 occurrences in `src/`
- `cargo fmt --check`: 9 cosmetic formatting differences in 3 files (lock.rs, state.rs test code, main.rs imports). These are minor style differences in test code and import grouping, not functional issues. The project does not appear to enforce `cargo fmt` in CI, and these diffs are trivial (e.g., method chain wrapping, type annotation line breaks, import ordering).

### Check 3: Commit Quality

**Status:** PASS

All 10 commits follow conventional commit format:
1. `feat(server): add backup name path traversal validation`
2. `fix(lock): eliminate TOCTOU race in PidLock::acquire via O_CREAT|O_EXCL`
3. `feat(server): replace single-slot current_op with running_ops HashMap`
4. `feat(server): wire CancellationToken into all 11 route handlers via tokio::select!`
5. `refactor(server): extract DRY run_operation helper for all route handlers`
6. `fix(server): make reload update AppState config+clients and watch loop clients`
7. `feat(list): add auto-retention after upload for CLI and API handlers`
8. `docs(main): clarify create --resume as intentionally deferred design decision`
9. `test: add integration tests T4-T10`
10. `docs(server): update CLAUDE.md for running_ops, run_operation, kill, validation, reload changes`

No WIP commits, no AI references.

### Check 4: Change Statistics

**Status:** PASS

- **8 files changed**, 1840 insertions, 857 deletions
- **Net:** +983 lines across functional code, tests, and documentation
- Key files: `routes.rs` (-602 lines from DRY refactor), `state.rs` (+476 lines with new functionality + tests), `list.rs` (+66 lines for auto-retention), `lock.rs` (+77 lines for atomic creation), `watch/mod.rs` (+29 lines for async reload with client recreation), `test/run_tests.sh` (+466 lines for 7 new integration tests)

### Check 5: Test Coverage

**Status:** PASS

- **542 unit tests pass** (0 failures, 0 ignored)
- **6 integration config tests pass**
- New tests added for: validation (6 tests), running_ops (4 tests), cancellation (2 tests), run_operation (2 tests), atomic lock (1 test)
- 7 new integration test functions in `test/run_tests.sh` (syntax validated)

### Check 6: Dependencies

**Status:** PASS

No changes to `Cargo.toml` or `Cargo.lock`. All new functionality uses existing crate features (`tokio_util::sync::CancellationToken`, `std::collections::HashMap`, `std::fs::OpenOptions::create_new`).

### Check 7: Rust Best Practices

**Status:** PASS (minor note)

- No `Arc<Box<...>>` anti-patterns in public APIs
- Proper use of `tokio::sync::Mutex` for async context
- `spawn_blocking` used correctly for sync functions (`retention_local`, `delete_local`, `clean_broken_local`)
- `cargo fmt --check` shows minor formatting differences (see Check 2)

### Check 8: Architecture

**Status:** PASS

- Event-driven model preserved: `tokio::select!` for cancellation, channel-based reload signaling
- No polling patterns introduced
- `run_operation` helper correctly encapsulates the spawn/select/metrics/lifecycle pattern
- HashMap-based tracking properly replaces single-slot Option

### Check 9: Component Wiring

**Status:** PASS

All new components are fully connected:
- `validate_backup_name`: Wired into 8 API routes + 2 CLI functions (confirmed via grep: 8 call sites in routes.rs, 2 in main.rs)
- `running_ops HashMap`: Used by try_start_op, finish_op, fail_op, kill_op, status handler, refresh_backup_counts
- `run_operation`: Used by 10 of 11 route handlers (post_actions excluded with documented reason)
- `apply_retention_after_upload`: Wired into CLI upload + create_remote (main.rs: 2 sites), API upload + create_remote + post_actions upload/create_remote branches (routes.rs: 4 sites)
- `reload_config_and_clients`: Called from both reload() and restart() handlers (2 sites)
- `apply_config_reload` (async): Called from interruptible_sleep with `.await`

### Check 10: Data Flow

**Status:** PASS

Complete data flows verified:
- Kill: `POST /api/v1/kill?id=N` -> `kill_op(Some(N))` -> `running_ops.remove(N)` -> `cancel_token.cancel()` -> `tokio::select!` cancelled branch fires -> `fail_op(id, "killed by user")`
- Reload: `POST /api/v1/reload` -> `reload_config_and_clients()` -> `Config::load + validate + ChClient::new + S3Client::new` -> `ArcSwap::store` for all three + optional watch reload signal
- Auto-retention: `upload_backup` closure -> `upload::upload()` -> `apply_retention_after_upload(&config, &s3, Some(&cache))` -> `retention_local` + `retention_remote` (best-effort)

### Check 11: Runtime Smoke Test

**Status:** PASS (alternative verification)

All runtime layers in acceptance.json are `not_applicable` with `alternative_verification` fields. The alternatives are static checks (grep patterns, test execution) which have been verified:
- F001: `validate_backup_name` tests pass
- F003: `current_op` absent from all source files (0 occurrences)
- F004: `_token` absent from routes.rs (0 occurrences); only in auto_resume (3 occurrences, intentional)
- F005: `try_start_op` appears exactly 1 time in routes.rs (post_actions only)
- F007: `apply_retention_after_upload` called 6 times across main.rs and routes.rs

### Check 12: Pattern Compliance

**Status:** PASS

Verified against `context/patterns.md`:
- Pattern 1 (Operation Lifecycle): Updated from `_token` discarding to proper `tokio::select!` wiring via `run_operation` helper
- Pattern 2 (AppState Operation Management): `current_op: Option<RunningOp>` replaced with `running_ops: HashMap<u64, RunningOp>` -- zero `current_op` references remain
- Pattern 4 (Watch Loop): `apply_config_reload` now async, recreates clients, updates atomically
- Pattern 5 (Config Hot-Reload): API reload now updates AppState config+clients via shared helper

---

## Phase 2: Design Review (6 Checks)

### Check 13: Plan Alignment

**Status:** PASS

All 10 tasks implemented as specified in PLAN.md:
- Task 1: validate_backup_name with 5 rejection rules, wired into API + CLI
- Task 2: Atomic O_CREAT|O_EXCL lock acquisition with stale lock recovery
- Task 3: HashMap<u64, RunningOp> replacing Option<RunningOp>
- Task 4: CancellationToken wired via tokio::select! in all 11 handlers
- Task 5: run_operation helper extracts boilerplate from 10 handlers (post_actions excluded per plan)
- Task 6: reload_config_and_clients shared helper, ArcSwap store, async apply_config_reload
- Task 7: apply_retention_after_upload in CLI and API (not watch, not auto_resume)
- Task 8: Comment documenting create --resume as intentionally deferred
- Task 9: 7 integration test functions, bash -n syntax valid
- Task 10: src/server/CLAUDE.md updated, zero stale current_op references

### Check 14: Code Quality Assessment

**Status:** PASS

- **DRY**: The `run_operation` helper eliminates ~600 lines of duplicated boilerplate across route handlers
- **Error handling**: All error paths properly mapped to HTTP status codes (400, 404, 423, 500)
- **Concurrency safety**: HashMap protected by tokio::Mutex, semaphore permit held for operation duration
- **No resource leaks**: OwnedSemaphorePermit dropped when RunningOp removed from HashMap
- **Documentation**: Code comments explain non-obvious decisions (post_actions exclusion, auto_resume _token, create --resume deferral)

### Check 15: Architecture / SOLID Review

**Status:** PASS

- **Single Responsibility**: `run_operation` encapsulates orchestration; closures contain business logic
- **Open/Closed**: New handlers can be added by calling `run_operation` without modifying it
- **Liskov**: Not applicable (no inheritance)
- **Interface Segregation**: `validate_backup_name` is a standalone function, not tied to AppState
- **Dependency Inversion**: Route handlers depend on the abstract `run_operation` interface, not concrete lifecycle management

**Design quality notes:**
- The `reload_config_and_clients` helper properly separates concerns (load vs. swap)
- Watch loop client recreation failure is non-fatal (keeps old clients) -- correct resilience pattern
- Kill drops tasks without cleanup -- documented as known limitation with `clean_shadow` workaround

### Check 16: Documentation

**Status:** PASS

- `src/server/CLAUDE.md`: Fully updated with 19 references to new patterns (running_ops, run_operation, validate_backup_name, kill endpoint, reload semantics, auto-retention)
- Zero stale `current_op` references in documentation
- PLAN.md architecture assumptions and risk assessment are accurate
- Code comments document key decisions inline

### Check 17: Issue Identification

**Issues found:**

| Severity | Issue | Location | Impact |
|----------|-------|----------|--------|
| Minor | `cargo fmt` shows formatting differences in 3 files | lock.rs:51, state.rs (tests), main.rs (imports) | Cosmetic only, no functional impact |

No critical or important issues found.

### Check 18: Verdict

**PASS**

This is a well-structured, comprehensive implementation that fixes multiple correctness issues in the server API:

1. **Security**: Path traversal validation prevents malicious backup names at all entry points
2. **Correctness**: HashMap-based operation tracking replaces lossy single-slot Option; kill endpoint now actually cancels operations via CancellationToken; PID lock TOCTOU race eliminated
3. **Maintainability**: DRY helper reduces ~600 lines of duplicated boilerplate; shared reload helper eliminates code duplication between reload and restart
4. **Completeness**: Auto-retention after upload fills a design doc gap (section 3.6 step 7); watch loop reload now recreates clients

The single minor issue (formatting) is cosmetic and does not block merge. All 542 tests pass, all 10 acceptance criteria are met, and the codebase is cleaner than before thanks to the DRY refactoring.

---

## Critical Items Verification

| Item | Expected | Actual | Status |
|------|----------|--------|--------|
| `current_op` absent from src/ | 0 occurrences | 0 occurrences | PASS |
| `_token` absent from routes.rs | 0 occurrences | 0 occurrences | PASS |
| `_token` only in auto_resume (state.rs) | 3 occurrences | 3 (lines 438, 483, 522) | PASS |
| `try_start_op` in routes.rs | 1 (post_actions) | 1 (line 263) | PASS |
| `validate_backup_name` in routes.rs | 8 call sites | 8 call sites | PASS |
| `validate_backup_name` in main.rs | 2 call sites | 2 (lines 676, 689) | PASS |
| `apply_retention_after_upload` call sites | 6 total | 7 total (2 main.rs + 4 routes.rs + 1 list.rs def) | PASS |
| `reload_config_and_clients` in routes.rs | 2 (reload + restart) | 2 (lines 1298, 1339) | PASS |
| `apply_config_reload` is async | yes | yes (line 605: `async fn`) | PASS |
| `apply_config_reload` recreates clients | yes | yes (lines 643-662: ChClient::new + S3Client::new) | PASS |
| `run_operation` calls in routes.rs | 10 | 10 | PASS |
| `token.cancelled()` total coverage | 11 handlers | 1 (helper) + 1 (post_actions) = 11 handlers | PASS |
