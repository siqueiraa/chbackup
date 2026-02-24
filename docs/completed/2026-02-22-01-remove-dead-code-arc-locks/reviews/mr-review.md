# MR Review: 2026-02-22-01-remove-dead-code-arc-locks

## Summary
- **Branch:** `claude/2026-02-22-01-remove-dead-code-arc-locks`
- **Base:** `master`
- **Commits:** 4
- **Files changed:** 7
- **Lines:** +2 / -54
- **Reviewer:** Claude (Codex fallback -- model not supported with ChatGPT account)

---

## 1. Plan Completeness
- All 4 features: status="pass" (F001, F002, F003, FDOC)
- All features have 4 verification layers each
- SESSION.md task status consistent with acceptance.json

RESULT: PASS

## 2. Code Quality
- Clippy: 0 warnings (`cargo clippy --all-targets -- -D warnings`)
- No `todo!()` or `unimplemented!()` outside tests
- No `#[allow(dead_code)]` outside tests (this plan specifically removed 2 such annotations)
- No `// TODO`, `// FIXME`, `// HACK` comments outside tests
- No placeholder/stub functions outside tests
- Note: grep found "stub" in a comment at `src/server/routes.rs:1131` and "substituting" in `src/watch/mod.rs:26` -- these are descriptive comments, not stub code

RESULT: PASS

## 3. Commit Quality
- 4 commits, all conventional format:
  - `5b912ba4 refactor(clickhouse): remove dead debug field and unused inner() getter from ChClient`
  - `9edc6e3c refactor(storage): remove unused inner(), concurrency(), object_disk_path() getters and dead fields from S3Client`
  - `c6152ac8 refactor(restore): remove dead attach_parts() function superseded by attach_parts_owned()`
  - `fbc227e4 docs: update CLAUDE.md files to reflect dead code removal from ChClient, S3Client, and attach.rs`
- No WIP/fixup/squash/tmp commits
- Atomic changes: each commit targets one module

RESULT: PASS

## 4. Change Statistics
- Files changed: 7
- Lines: +2 / -54 (net deletion of 52 lines)
- Largest changes:
  - `src/storage/s3.rs`: -26 lines (removed 3 getters, 2 fields, 2 test helper fields)
  - `src/clickhouse/client.rs`: -13 lines (removed 1 field, 1 getter)
  - `src/restore/attach.rs`: -11 lines (removed 1 function)
  - `CLAUDE.md`: +2/-1 (documentation update for S3 fields note)
  - 3 CLAUDE.md files: -1 line each (removed dead API docs)
- No large files, no splitting needed

RESULT: PASS

## 5. Test Coverage
- Tests: 542 passed, 0 failed, 0 ignored
- No test files deleted in commits
- No test count decrease (pure dead-code removal does not require new tests)

RESULT: PASS

## 6. Dependencies
- No changes to `Cargo.toml` or `Cargo.lock`
- No new dependencies

RESULT: PASS

## 7. Rust Best Practices
- `cargo fmt -- --check`: formatted (exit 0)
- No `Arc/Box/Rc` in new public APIs (no new public APIs added)
- No new public types or functions (deletion only)
- No security audit needed (no dependency changes)

RESULT: PASS

## 8. Architecture
- Sound design: removes genuinely dead code confirmed via LSP findReferences and grep
- Follows zero-warnings policy by removing `#[allow(dead_code)]` suppression
- No unnecessary complexity (pure deletion)
- Error handling: not applicable (no new error paths)
- No performance implications
- No event-driven patterns affected
- No state persistence affected
- No actor files changed

RESULT: PASS

## 9. Component Wiring
- No new actors, structs, or message types added
- No public functions added
- All deletions verified as unused:
  - `ChClient::inner()`: 0 external callers (LSP verified)
  - `ChClient.debug`: field stored but never read
  - `S3Client::inner()`: 0 external callers
  - `S3Client::concurrency()`: 0 external callers
  - `S3Client::object_disk_path()`: 0 external callers
  - `S3Client.concurrency`: field stored but never read (config field retained)
  - `S3Client.object_disk_path`: field stored but never read (config field retained)
  - `attach_parts()`: 0 callers (superseded by `attach_parts_owned()`)

RESULT: PASS

## 10. Data Flow
- No data flow changes (pure deletion)
- No new inputs, outputs, or processing
- No data "dead ends" introduced

RESULT: PASS

## 11. Runtime Smoke Test
- Runtime layer: `not_applicable` for all 4 features
- Justification: Pure dead-code removal with no behavioral change
- Alternative verification executed:
  - `cargo check` with 0 warnings
  - `cargo test -p chbackup`: 542 tests passed
- Evidence documented in `reviews/runtime-verify.md`
- No "deferred" or "skipped" in evidence

RESULT: PASS

## 12. Pattern Compliance
- Plan-local `context/patterns.md` exists
- Implementation follows Pattern 2 (removing unused public APIs) exactly
- Implementation follows Pattern 1 (removing `#[allow(dead_code)]` annotations)
- Pattern 3 (Arc wrapping) correctly left untouched
- Pattern 4 (unused metrics) correctly left untouched
- Pattern 5 (unused error variants) correctly left untouched

RESULT: PASS

---

## 13. Plan Alignment
- Implementation matches PLAN.md approach exactly
- All 4 tasks completed as specified
- No deviations from plan
- No planned features silently dropped
- Config fields `s3.concurrency` and `s3.object_disk_path` correctly retained (only struct fields removed)

RESULT: PASS

## 14. Code Quality (Design)
- Proper error handling maintained (no changes)
- No type safety regression
- Naming conventions maintained
- Code organization improved (dead code removed)
- Test coverage maintained (test helper updated for S3Client field removal)

RESULT: PASS

## 15. Architecture & SOLID
- Single Responsibility: unchanged
- Open/Closed: unchanged (removal only)
- No new coupling or abstractions
- Integration with existing codebase preserved

RESULT: PASS

## 16. Documentation
- `src/clickhouse/CLAUDE.md`: `inner()` getter removed from Public API section
- `src/storage/CLAUDE.md`: `inner()` getter removed from Public API section
- `src/restore/CLAUDE.md`: `attach_parts()` removed from Public API section
- Root `CLAUDE.md`: S3 concurrency + object_disk_path bullet updated to note fields/getters removed
- Root `CLAUDE.md`: Post-review correctness fixes entry added (from prior branch work)

RESULT: PASS

## 17. Issues Found

### Critical (Must Fix)
None

### Important (Must Fix)
None

### Minor (Must Fix)
None

## 18. Issue Summary
- Critical: 0
- Important: 0
- Minor: 0

---

## Verdict

**PASS**

**Reasoning:** This is a clean dead-code removal plan that deletes 52 lines of genuinely unused code (verified via LSP findReferences and grep). All 4 acceptance criteria pass, all 542 tests pass, clippy reports 0 warnings, formatting is clean, no dependencies changed, documentation updated correctly, and no behavioral changes were made. The plan correctly preserves config fields while removing only the dead struct fields and unused getters.
