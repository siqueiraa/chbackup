# Data Authority Analysis

## Context

This plan adds unit tests and raises the CI coverage gate. No new data tracking or calculations are being introduced. The "data" in question is test coverage metrics and the CI threshold.

## Data Sources

| Data Needed | Source | Field Available | Decision |
|-------------|--------|-----------------|----------|
| Current coverage % | `cargo llvm-cov` output | TOTAL line in summary | USE EXISTING |
| CI gate threshold | `.github/workflows/ci.yml:71` | `assert float(...) >= 35` literal | USE EXISTING - modify in-place |
| Function signatures | Source files | `fn` declarations | USE EXISTING - verified via grep |
| Existing test coverage per file | `cargo llvm-cov` per-file output | Per-file line coverage | USE EXISTING |
| Which functions are pure | Manual source code analysis | No external data needed | N/A - human analysis |

## Analysis Notes

- Coverage data comes from `cargo llvm-cov test --all --all-features --summary-only`
- The CI gate is a Python assert in a shell step (line 71 of ci.yml)
- No external data sources, APIs, or databases are involved in this plan
- All function testability was determined by reading the actual source code, not documentation
- There is no "MUST IMPLEMENT" data -- all data needed already exists
