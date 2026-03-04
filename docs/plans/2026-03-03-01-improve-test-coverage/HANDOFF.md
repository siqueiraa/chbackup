# Handoff: Improve Test Coverage Quality Signal

## Plan Location
`docs/plans/2026-03-03-01-improve-test-coverage/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps for 5 tasks across 3 dependency groups |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 5-layer verification criteria (F001-F005) |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Test patterns discovered in codebase |
| context/symbols.md | Type verification for all testable functions |
| context/diagnostics.md | Baseline: 0 errors, 0 warnings, 1049 tests passing |
| context/references.md | Function signatures and caller analysis |
| context/git-history.md | Recent commit history and coverage-related commits |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Module status (no CLAUDE.md updates needed) |
| context/redundancy-analysis.md | N/A - no new components |
| context/preventive-rules-applied.md | Applied rules verification |
| context/data-authority.md | Data source verification |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/main.rs` -- Adding `#[cfg(test)] mod tests` block with ~20 tests for pure helper functions (backup_name_from_command, resolve_backup_name, backup_name_required, map_cli_location, map_cli_list_format, merge_skip_projections)
- `src/backup/mod.rs` -- Extending existing test module with ~7 NEW edge-case tests (is_benign_type_enum16, nested_nullable_array_tuple_is_false, map_type, lowertuple, normalize_uuid_whitespace_is_some, partial_zeros, filter_mixed_keeps). NOT duplicating existing normalize_uuid/is_benign_type tests.
- `src/download/mod.rs` -- Extending existing test module with ~8 tests for sanitize_relative_path (SECURITY-CRITICAL, currently 0 tests)
- `src/restore/attach.rs` -- Extending existing test module with ~8 tests for is_attach_warning only (currently 0 tests). NOT duplicating existing hardlink/ownership/uuid tests.
- `.github/workflows/ci.yml` -- Raising coverage gate threshold from 35% to 55%

### Test Patterns
- All tests follow inline `#[cfg(test)] mod tests { use super::*; }` pattern
- Pure function tests: simple input/output assertions
- Filesystem tests: use `tempfile::tempdir()`
- Error tests: construct `anyhow::anyhow!("message")` and pass by reference
- No mocking framework; no async tests needed

### Key Constraints
- main.rs functions are PRIVATE to binary crate -- tests MUST be inline
- cli::Command enum variants require ALL fields to construct (10+ fields per variant)
- No ChClient/S3Client dependencies in any target function
- Current coverage: 66.68%, target CI gate: 55% (safe headroom)

### Related Documentation
- `context/symbols.md` -- Complete function signature verification table
- `context/knowledge_graph.json` -- Machine-readable symbol lookup
- `context/patterns.md` -- Test pattern reference implementations
