# Diagnostics

## Cargo Check Status
- Last run: pre-plan creation
- Status: Clean (zero warnings, zero errors on master branch)

## Key Compiler Observations
- All 35 gap items involve EXISTING config fields that are parsed but never used
- No new struct definitions needed (except possibly for actions dispatch)
- `aws-sdk-sts` dependency needed for Task 3 (STS AssumeRole)
- `fastrand` or `rand` crate needed for Task 5 (jitter)
- `uuid` crate already available (used by server)

## Module Sizes (lines)
| File | Lines | Tasks |
|------|-------|-------|
| src/server/routes.rs | 2024 | 10 |
| src/list.rs | 1758 | 9, 12 |
| src/restore/mod.rs | 1278 | 7 |
| src/storage/s3.rs | 1027 | 2, 3, 4, 5, 8 |
| src/backup/mod.rs | 926 | 6 |
| src/main.rs | 685 | 12 |
| src/config.rs | ~1200 | 1 |
| src/cli.rs | 333 | 12 |
