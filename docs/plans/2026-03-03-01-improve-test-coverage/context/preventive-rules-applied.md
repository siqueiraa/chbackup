# Preventive Rules Applied

## Rules Checked

| Rule | Applicable? | Action |
|------|-------------|--------|
| RC-001 | No | No actors in this project (plain async Rust, no kameo) |
| RC-002 | Yes | Verified all types via source code reading, not comments |
| RC-004 | No | No message handlers; this plan adds tests, not messages |
| RC-006 | Yes | Verified all function signatures exist via grep before listing |
| RC-007 | Yes | Verified struct field order via source reading (ColumnInconsistency, etc.) |
| RC-008 | Yes | TDD sequencing: tests reference existing functions only, no new structs needed |

## Planning Rules Applied

| Rule | Check |
|------|-------|
| PR-001: Verify types exist | All function signatures verified via grep + file reads |
| PR-002: Check function signatures | Every testable function's exact signature confirmed in source |
| PR-003: No phantom features | Plan only adds #[cfg(test)] modules to existing files |
| PR-004: Verify imports | All types used (ColumnInconsistency, cli::Location, etc.) verified in source |

## Key Observations

- This plan adds ONLY test code (#[cfg(test)] blocks) to existing files
- No new public API, no new structs, no new modules
- All functions being tested already exist and have verified signatures
- The CI gate change is a one-line edit to an existing value
- No async code is being added (all new tests are sync #[test])
