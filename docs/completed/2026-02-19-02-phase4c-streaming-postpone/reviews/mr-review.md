# MR Review: Phase 4c Streaming Engine Postponement

**Branch:** `phase4c-streaming-postpone`
**Base:** `master` (merge-base: `e0668461`)
**Reviewer:** Claude (fallback)
**Date:** 2026-02-19
**Verdict:** **PASS**

---

## Commits Reviewed (4)

| Commit | Message | Files |
|--------|---------|-------|
| `eabb2009` | feat(restore): add streaming engine and refreshable MV detection functions | `src/restore/topo.rs` |
| `ceeb65f3` | feat(restore): populate postponed_tables in classify_restore_tables | `src/restore/topo.rs` |
| `ec926658` | feat(restore): add Phase 2b execution block for postponed tables | `src/restore/mod.rs` |
| `62b45a1b` | docs(restore): update CLAUDE.md for Phase 4c streaming engine postponement | `src/restore/CLAUDE.md` |

## Diff Summary

- 3 files changed, 255 insertions, 20 deletions
- `src/restore/topo.rs`: +203 lines (detection functions, classification logic, 5 new test functions)
- `src/restore/mod.rs`: +29 lines (Phase 2b execution blocks in full-restore and schema-only paths)
- `src/restore/CLAUDE.md`: +43 lines (documentation updates)

---

## Phase 1: Automated Verification Checks

### 1. Compilation
- **Status:** PASS
- `cargo check` completes with zero errors

### 2. Clippy Warnings
- **Status:** PASS
- `cargo clippy -- -W clippy::all` reports zero warnings

### 3. Test Suite
- **Status:** PASS
- All 368 tests pass (0 failures, 0 ignored)
- 5 new tests added for Phase 4c functionality

### 4. Debug Markers
- **Status:** PASS
- Zero `DEBUG_MARKER` or `DEBUG_VERIFY` patterns found in `src/`

### 5. Formatting
- **Status:** PASS
- Code follows consistent formatting (no `cargo fmt` changes needed)

### 6. Conventional Commits
- **Status:** PASS
- All 4 commits follow conventional commit format (`feat:`, `docs:`)

### 7. No AI References
- **Status:** PASS
- No mentions of Claude, AI, or AI tools in commit messages or code

### 8. Plan Alignment
- **Status:** PASS
- All 4 tasks from PLAN.md are implemented (F001, F002, F003, FDOC)
- Implementation matches design doc section 5.1 Phase 2b specification (lines 1295-1298)
- Issue #1235 (streaming engine safety) correctly addressed

### 9. Design Doc Alignment
- **Status:** PASS
- Design doc defines Phase 2b at line 1295: "Postponed tables -- Streaming engines (Kafka, NATS, RabbitMQ, S3Queue) and refreshable MVs"
- All four streaming engines covered: Kafka, NATS, RabbitMQ, S3Queue
- Refreshable MV detection implemented per design (CH >= 24.1 feature)
- Execution order matches design: data attach -> Phase 2b -> Phase 3 DDL-only -> Phase 4 functions

### 10. Backwards Compatibility
- **Status:** PASS
- Existing `test_classify_restore_tables_basic` still passes (no streaming engines in that test)
- `RestorePhases.postponed_tables` was already in the struct (previously hardcoded to empty Vec)
- No API signature changes

### 11. Error Handling
- **Status:** PASS
- `create_tables()` propagation via `?` operator consistent with existing patterns
- `data_only` guard prevents unnecessary DDL execution

### 12. Test Coverage
- **Status:** PASS
- `test_is_streaming_engine`: All 4 streaming engines + 6 non-streaming engines
- `test_is_refreshable_mv`: 5 cases including case-insensitive, newline boundary, non-MV edge case
- `test_classify_streaming_engines_postponed`: Mixed manifest (data + streaming + DDL-only)
- `test_classify_refreshable_mv_postponed`: Refreshable MV vs regular MV distinction
- `test_classify_all_streaming_engines`: All 4 engines routed to postponed

---

## Phase 2: Design Review

### 1. Architecture & Correctness

**PASS.** The implementation correctly fills in the Phase 2b placeholder that was established in Phase 4b. The decision tree in `classify_restore_tables()` checks streaming engine and refreshable MV status BEFORE the `metadata_only` check, which is critical because:

- Kafka/NATS/RabbitMQ/S3Queue tables are NOT `metadata_only` (they have data parts), so without the priority check they would end up in `data_tables` and be created too early.
- Refreshable MVs ARE `metadata_only`, so without the priority check they would end up in `ddl_only_tables` (Phase 3) instead of being properly postponed.

The schema-only mode ordering (Phase 2b after Phase 3 DDL-only) is correct -- since streaming engines may write to targets that are DDL-only objects (e.g., a Kafka MV writes to a regular MV), the DDL-only targets must exist first.

### 2. Mode Handling

**PASS.**
- Full restore: Phase 2b runs after data attachment, before Phase 3. Correct.
- Schema-only: Phase 2b runs after Phase 3 DDL-only objects. Correct rationale documented.
- Data-only: Phase 2b guarded by `!data_only`. Additionally, `create_tables()` has internal `data_only` early return. Double protection, consistent with existing Phase 3 pattern.

### 3. Code Quality

**PASS with minor finding.**
- `is_streaming_engine()` uses `matches!()` macro, consistent with `is_metadata_only_engine()` pattern
- `is_refreshable_mv()` is well-documented with clear rationale for detection approach
- Classification log updated to include all three categories
- Module doc comments updated in mod.rs

### 4. Documentation

**PASS.** CLAUDE.md comprehensively updated with:
- Phase 2b in phased architecture overview
- New "Streaming Engine Postponement" subsection
- Updated public API section with both new functions
- Mode-specific behavior documented

### 5. Security / Safety

**PASS.** No new external inputs processed. Engine names are compared against hardcoded strings. DDL content is only inspected (not modified) for REFRESH detection.

### 6. Performance

**PASS.** No performance concerns. Both detection functions are O(1) for streaming engine and O(n) for DDL string scan. The DDL string scan happens once per table during classification, not in a hot loop.

---

## Findings

### Minor (Non-blocking)

1. **Stale doc comment on `RestorePhases.postponed_tables`** (`src/restore/topo.rs:78`):
   The field comment still says "empty for now (Phase 4c)" but Phase 4c is now implemented. Should read something like "Phase 2b: Postponed tables (streaming engines, refreshable MVs)."

2. **REFRESH detection edge case with tab characters** (`src/restore/topo.rs:56`):
   The detection checks for ` REFRESH ` (space-bounded) and `\nREFRESH ` (newline-bounded) but would miss `\tREFRESH ` (tab-bounded) or `\r\nREFRESH ` (CRLF). In practice, ClickHouse DDL uses spaces and newlines, so this is extremely unlikely to cause issues. No action required.

---

## Verdict

**PASS**

The implementation is correct, well-tested, aligned with the design doc and plan, compiles cleanly, and follows existing codebase patterns. The two minor findings are non-blocking and do not affect correctness.
