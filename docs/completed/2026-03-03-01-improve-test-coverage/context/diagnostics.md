# Diagnostics Report

## Compiler State

**Date:** 2026-03-03
**Branch:** master
**Commit:** 421cc296

### cargo check
```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 6.40s
```
- **Errors:** 0
- **Warnings:** 0

### cargo clippy --all-targets
```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.91s
```
- **Errors:** 0
- **Warnings:** 0

### cargo test
```
test result: ok. 1035 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out
test result: ok. 6 passed; 0 failed; 0 ignored (doc tests - lib)
test result: ok. 6 passed; 0 failed; 0 ignored (doc tests - bin)
test result: ok. 2 passed; 0 failed; 0 ignored (doc tests - path_encoding)
```
- **Total tests:** 1049 (1035 unit + 14 doc tests)
- **Failures:** 0
- **Ignored:** 3

## CI Configuration

### Coverage Gate (ci.yml:66-71)
```yaml
- name: Unit test coverage gate
  run: |
    cargo llvm-cov test --all --all-features --summary-only 2>&1 | tee cov-summary.txt
    LINE_PCT=$(grep 'TOTAL' cov-summary.txt | awk '{print $10}' | tr -d '%')
    echo "Unit coverage: ${LINE_PCT}%"
    python3 -c "assert float('${LINE_PCT}') >= 35, f'Coverage ${LINE_PCT}% < 35%'"
```
- **Current gate:** 35%
- **Current coverage:** 66.68% (from discovery phase)
- **Gap:** 31.68 percentage points of headroom above gate

## Codebase Health Summary

- Clean compilation (zero errors, zero warnings)
- All 1049 tests passing
- No clippy lints
- Coverage gate is very conservative (35%) relative to actual coverage (66.68%)
- No existing compiler diagnostics to address
