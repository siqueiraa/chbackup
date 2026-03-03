# Plan: Improve Test Coverage Quality Signal and Increase Unit Test Coverage

## Goal

Add unit tests for untested pure functions across the codebase (main.rs, backup/mod.rs, download/mod.rs, restore/attach.rs) and raise the CI coverage gate from 35% to 55% to provide a meaningful quality signal. Baseline: 66.68% line coverage, 1049 tests.

## Architecture Overview

This plan adds ONLY test code (`#[cfg(test)]` modules) to existing source files and changes a single numeric threshold in CI. No production code is modified. All target functions are pure (no I/O, no async, no external dependencies) and can be tested with simple input/output assertions.

**Source files being modified:**
- `src/main.rs` -- Add new `#[cfg(test)] mod tests` block (file has none currently)
- `src/backup/mod.rs` -- Extend existing `mod tests` with new test functions
- `src/download/mod.rs` -- Extend existing `mod tests` with new test functions
- `src/restore/attach.rs` -- Extend existing `mod tests` with new test functions
- `.github/workflows/ci.yml` -- Change threshold 35 -> 55

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **main.rs helpers**: Private functions in binary crate. Tests MUST be inline `#[cfg(test)]` at the end of main.rs because these functions are not accessible from external test files.
- **backup/mod.rs helpers**: Private functions. Existing `#[cfg(test)] mod tests` block at line 909 uses `use super::*;` pattern.
- **download/mod.rs helpers**: Private functions. Existing `#[cfg(test)] mod tests` block at line 1163 uses `use super::*;` pattern.
- **restore/attach.rs helpers**: Mix of `pub` and private functions. Existing `#[cfg(test)] mod tests` block at line 1079 uses `use super::*;` pattern.
- **cli::Command enum**: Defined in `src/cli.rs` (private `mod cli;` in main.rs). Accessible from main.rs tests via `use super::*;`.

### What This Plan CANNOT Do
- Cannot unit-test functions requiring `&ChClient` or `&S3Client` (need real services)
- Cannot unit-test async functions that call S3 or ClickHouse
- Cannot raise main.rs coverage above ~12% because `run()` (the bulk of the file) is untestable without integration tests
- Cannot use mocking frameworks (project convention: no mocks)

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| CI gate too aggressive | GREEN | 55% threshold has ~12% headroom below current 66.68% |
| Breaking existing tests | GREEN | Only adding new tests, no code changes |
| main.rs test construction complexity | GREEN | cli::Command variants are well-documented; construct minimal instances |
| Coverage measurement flakiness | GREEN | cargo-llvm-cov is deterministic for unit tests |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| N/A | no | This plan adds only test code and a CI config change. No runtime binary behavior is modified. All verification is via `cargo test` and `cargo llvm-cov`. |

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| Low coverage in server/routes.rs (44%) | Requires AppState/integration testing | Future plan with test server setup |
| Low coverage in upload/mod.rs (39%) | Most code requires S3Client | Future integration test expansion |
| Low coverage in storage/s3.rs (28%) | Requires real S3 | Covered by integration tests |
| Remaining main.rs untested code | `run()` function needs full integration | Covered by docker-compose tests |

## Dependency Groups

```
Group A (Sequential -- main.rs tests):
  - Task 1: Add #[cfg(test)] mod tests to main.rs with tests for all pure helpers

Group B (Independent -- can run in parallel with A):
  - Task 2: Add tests for normalize_uuid and is_benign_type in backup/mod.rs
  - Task 3: Add tests for sanitize_relative_path in download/mod.rs
  - Task 4: Add tests for is_attach_warning and additional edge cases in restore/attach.rs

Group C (Sequential -- depends on A+B):
  - Task 5: Raise CI coverage gate from 35% to 55%
```

## Tasks

### Task 1: Add unit tests for pure helper functions in main.rs

**TDD Steps:**
1. Write `#[cfg(test)] mod tests` at end of `src/main.rs` with `use super::*;`
2. Write test `test_backup_name_from_command_create` -- construct `Command::Create` with `backup_name: Some("daily")`, assert returns `Some("daily")`
3. Write test `test_backup_name_from_command_create_none` -- construct `Command::Create` with `backup_name: None`, assert returns `None`
4. Write test `test_backup_name_from_command_upload` -- construct `Command::Upload`, assert extraction works
5. Write test `test_backup_name_from_command_download` -- construct `Command::Download`, assert extraction
6. Write test `test_backup_name_from_command_restore` -- construct `Command::Restore`, assert extraction
7. Write test `test_backup_name_from_command_create_remote` -- construct `Command::CreateRemote`, assert extraction
8. Write test `test_backup_name_from_command_restore_remote` -- construct `Command::RestoreRemote`, assert extraction
9. Write test `test_backup_name_from_command_delete` -- construct `Command::Delete`, assert extraction
10. Write test `test_backup_name_from_command_list_returns_none` -- `Command::List` returns `None`
11. Write test `test_backup_name_from_command_tables_returns_none` -- `Command::Tables` returns `None`
12. Write test `test_resolve_backup_name_with_valid_name` -- `resolve_backup_name(Some("daily-2024"))` returns `Ok("daily-2024")`
13. Write test `test_resolve_backup_name_generates_when_none` -- `resolve_backup_name(None)` returns `Ok(name)` where name is non-empty
14. Write test `test_resolve_backup_name_rejects_latest` -- `resolve_backup_name(Some("latest"))` returns `Err` containing "reserved"
15. Write test `test_resolve_backup_name_rejects_previous` -- `resolve_backup_name(Some("previous"))` returns `Err` containing "reserved"
16. Write test `test_resolve_backup_name_rejects_path_traversal` -- `resolve_backup_name(Some("../evil"))` returns `Err`
17. Write test `test_backup_name_required_with_name` -- `backup_name_required(Some("daily"), "upload")` returns `Ok("daily")`
18. Write test `test_backup_name_required_none_fails` -- `backup_name_required(None, "upload")` returns `Err` containing "required"
19. Write test `test_backup_name_required_rejects_invalid` -- `backup_name_required(Some("../bad"), "upload")` returns `Err`
20. Write test `test_map_cli_location_local` -- `map_cli_location(cli::Location::Local)` returns `list::Location::Local`
21. Write test `test_map_cli_location_remote` -- `map_cli_location(cli::Location::Remote)` returns `list::Location::Remote`
22. Write test `test_map_cli_list_format_all_variants` -- test all 5 variants map correctly
23. Write test `test_merge_skip_projections_cli_takes_precedence` -- `merge_skip_projections(Some("a,b"), &["c".to_string()])` returns `["a", "b"]`
24. Write test `test_merge_skip_projections_falls_back_to_config` -- `merge_skip_projections(None, &["x".to_string()])` returns `["x"]`
25. Write test `test_merge_skip_projections_empty` -- `merge_skip_projections(None, &[])` returns empty vec
26. Write test `test_merge_skip_projections_trims_whitespace` -- `merge_skip_projections(Some(" a , b "), &[])` returns `["a", "b"]`
27. Write test `test_merge_skip_projections_filters_empty_parts` -- `merge_skip_projections(Some("a,,b,"), &[])` returns `["a", "b"]`
28. Verify all tests pass: `cargo test --bin chbackup`

**Files:** `src/main.rs`
**Acceptance:** F001

**Implementation Notes:**
- `Command::Create` requires all 10 fields. Use `backup_name: Some("test".to_string())` and `false`/`None` defaults for all others.
- `Command::List` requires `location: Option<Location>` and `format: ListFormat`. Use `None` and `ListFormat::Default`.
- For `resolve_backup_name(None)` test, just check `result.is_ok()` and that the name is non-empty (timestamp is non-deterministic).
- The `validate_backup_name` function is imported from `crate::server::state` via `use chbackup::server::state::validate_backup_name;` at the top of main.rs.
- `map_cli_location` and `map_cli_list_format` need `cli::Location` and `cli::ListFormat` from the private `cli` module -- accessible via `use super::*;` because `mod cli;` is declared in main.rs.

### Task 2: Add tests for normalize_uuid and is_benign_type in backup/mod.rs

**TDD Steps:**
1. Write test `test_normalize_uuid_valid` -- `normalize_uuid("abc-123-def")` returns `Some("abc-123-def")`
2. Write test `test_normalize_uuid_empty` -- `normalize_uuid("")` returns `None`
3. Write test `test_normalize_uuid_all_zeros` -- `normalize_uuid("00000000-0000-0000-0000-000000000000")` returns `None`
4. Write test `test_normalize_uuid_non_zero` -- `normalize_uuid("5f3a7b2c-1234-5678-9abc-def012345678")` returns `Some(...)`
5. Write test `test_is_benign_type_enum_variants` -- `is_benign_type("Enum8('a' = 1)")` returns `true`
6. Write test `test_is_benign_type_enum16` -- `is_benign_type("Enum16('active' = 1, 'deleted' = 2)")` returns `true`
7. Write test `test_is_benign_type_tuple` -- `is_benign_type("Tuple(a Int32, b String)")` returns `true`
8. Write test `test_is_benign_type_nullable_enum` -- `is_benign_type("Nullable(Enum8('a' = 1))")` returns `true`
9. Write test `test_is_benign_type_nullable_tuple` -- `is_benign_type("Nullable(Tuple(x Int32))")` returns `true`
10. Write test `test_is_benign_type_array_tuple` -- `is_benign_type("Array(Tuple(a Int32, b Int32))")` returns `true`
11. Write test `test_is_benign_type_non_benign` -- `is_benign_type("Int32")` returns `false`
12. Write test `test_is_benign_type_non_benign_string` -- `is_benign_type("String")` returns `false`
13. Write test `test_is_benign_type_non_benign_nullable_int` -- `is_benign_type("Nullable(Int32)")` returns `false`
14. Write test `test_filter_benign_type_drift_removes_all_benign` -- vec with all benign types filtered to empty
15. Write test `test_filter_benign_type_drift_keeps_non_benign` -- vec with `["Int32", "String"]` types kept
16. Write test `test_filter_benign_type_drift_mixed` -- vec with one benign-only and one mixed, keeps only mixed
17. Verify: `cargo test --lib -- backup::tests`

**Files:** `src/backup/mod.rs`
**Acceptance:** F002

**Implementation Notes:**
- `ColumnInconsistency` is imported from `crate::clickhouse::client::ColumnInconsistency` (already in scope via `use super::*;` since `mod.rs` imports from `crate::clickhouse`).
- Existing tests already construct `ColumnInconsistency` directly (see lines ~1060 in existing test module). Follow same pattern.
- `is_benign_type` and `filter_benign_type_drift` are private functions accessible via `use super::*;`.

### Task 3: Add tests for sanitize_relative_path in download/mod.rs (SECURITY-CRITICAL)

**TDD Steps:**
1. Write test `test_sanitize_relative_path_normal` -- `sanitize_relative_path("metadata/db/table.json")` returns `PathBuf::from("metadata/db/table.json")`
2. Write test `test_sanitize_relative_path_parent_traversal` -- `sanitize_relative_path("../../etc/passwd")` returns `PathBuf::from("etc/passwd")` (strips `..` components)
3. Write test `test_sanitize_relative_path_absolute` -- `sanitize_relative_path("/etc/passwd")` returns `PathBuf::from("etc/passwd")` (strips root)
4. Write test `test_sanitize_relative_path_curdir` -- `sanitize_relative_path("./some/./path")` returns `PathBuf::from("some/path")` (strips `.`)
5. Write test `test_sanitize_relative_path_mixed_attack` -- `sanitize_relative_path("/../../../tmp/evil")` returns `PathBuf::from("tmp/evil")`
6. Write test `test_sanitize_relative_path_empty` -- `sanitize_relative_path("")` returns `PathBuf::from("")` (empty path)
7. Write test `test_sanitize_relative_path_single_normal` -- `sanitize_relative_path("file.txt")` returns `PathBuf::from("file.txt")`
8. Write test `test_sanitize_relative_path_double_dot_in_name` -- `sanitize_relative_path("dir/..hidden/file")` returns path with `..hidden` preserved (it is a Normal component, not ParentDir)
9. Verify: `cargo test --lib -- download::tests`

**Files:** `src/download/mod.rs`
**Acceptance:** F003

**Implementation Notes:**
- This function is SECURITY-CRITICAL (prevents path traversal via crafted S3 object keys).
- Tests must cover: normal paths, `..` traversal, absolute paths, `.` current dir, mixed attacks, and edge cases.
- The function uses `Path::components()` filter for `Component::Normal` only.
- `"..hidden"` is parsed as a `Normal` component by Rust's `Path`, not as `ParentDir` (only bare `..` is `ParentDir`).

### Task 4: Add tests for is_attach_warning and edge cases in restore/attach.rs

**TDD Steps:**
1. Write test `test_is_attach_warning_duplicate_data_part` -- `anyhow::anyhow!("Code: 232. DUPLICATE_DATA_PART")` returns `true`
2. Write test `test_is_attach_warning_part_temporarily_locked` -- `anyhow::anyhow!("PART_IS_TEMPORARILY_LOCKED")` returns `true`
3. Write test `test_is_attach_warning_no_such_data_part` -- `anyhow::anyhow!("NO_SUCH_DATA_PART")` returns `true`
4. Write test `test_is_attach_warning_code_232` -- `anyhow::anyhow!("Code: 232")` returns `true`
5. Write test `test_is_attach_warning_code_233` -- `anyhow::anyhow!("Code: 233")` returns `true`
6. Write test `test_is_attach_warning_other_error` -- `anyhow::anyhow!("Code: 60. UNKNOWN_TABLE")` returns `false`
7. Write test `test_is_attach_warning_connection_error` -- `anyhow::anyhow!("Connection refused")` returns `false`
8. Write test `test_is_attach_warning_empty_error` -- `anyhow::anyhow!("")` returns `false`
9. Write test `test_uuid_s3_prefix_short_uuid` -- `uuid_s3_prefix("ab")` returns `"store/ab/ab"` (short hex, < 3 chars)
10. Write test `test_uuid_s3_prefix_no_dashes` -- `uuid_s3_prefix("5f3a7b2c123456789abcdef012345678")` returns `"store/5f3/5f3a7b2c123456789abcdef012345678"` (preserves original, prefix from hex-only)
11. Write test `test_detect_clickhouse_ownership_tempdir` -- create tempdir, call `detect_clickhouse_ownership`, verify returns `Some(uid)` and `Some(gid)` for existing dir
12. Write test `test_hardlink_or_copy_dir_empty_src` -- create empty src dir, verify hardlink_or_copy_dir succeeds with empty dst
13. Verify: `cargo test --lib -- restore::attach::tests`

**Files:** `src/restore/attach.rs`
**Acceptance:** F004

**Implementation Notes:**
- `is_attach_warning` takes `&anyhow::Error`. Construct errors with `anyhow::anyhow!("message")` and pass `&error`.
- `uuid_s3_prefix` is `pub fn` -- already has 1 test but edge cases (short UUID, no dashes) are not covered.
- `detect_clickhouse_ownership` test should use `tempfile::tempdir()` and verify uid/gid are `Some` values.
- The existing test for `detect_clickhouse_ownership_nonexistent` covers the None case (line 1162).

### Task 5: Raise CI coverage gate from 35% to 55%

**TDD Steps:**
1. Edit `.github/workflows/ci.yml` line 71: change `>= 35` to `>= 55`
2. Verify the change is correct by inspecting the assertion line
3. Run `cargo test` to confirm all tests still pass (including new ones from Tasks 1-4)
4. Verify local coverage exceeds 55% (if cargo-llvm-cov is available)

**Files:** `.github/workflows/ci.yml`
**Acceptance:** F005

**Implementation Notes:**
- Single-line change: `assert float('${LINE_PCT}') >= 35` -> `assert float('${LINE_PCT}') >= 55`
- Current coverage is 66.68%, so 55% provides ~12% headroom for CI stability
- The new tests from Tasks 1-4 should push coverage slightly higher (~67-68%)

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All function signatures verified via source code reading (main.rs:24, main.rs:726, main.rs:744, main.rs:756, main.rs:764, main.rs:808, backup/mod.rs:43, backup/mod.rs:890, download/mod.rs:44, restore/attach.rs:132, restore/attach.rs:915, restore/attach.rs:1024) |
| RC-008 | PASS | All tests reference existing functions -- no new code is being added |
| RC-015 | N/A | No cross-task data flow -- each task is independent test additions |
| RC-016 | N/A | No new structs defined |
| RC-017 | N/A | No self.X usage -- tests call free functions |
| RC-018 | PASS | Every task has explicit test names, inputs, and assertions |

## Notes

### Phase 4.5 Skip Justification
Interface skeleton simulation (Phase 4.5) is skipped because this plan creates NO new production code. All changes are inside `#[cfg(test)]` blocks (compiled only during `cargo test`) and a CI config file. There are no new imports, types, or signatures to verify.

### Phase 4.6 CLAUDE.md Skip Justification
CLAUDE.md update task is skipped because all changes are test-only additions inside existing `#[cfg(test)] mod tests` blocks and a CI threshold change. No CLAUDE.md files need creation or update per `context/affected-modules.json`.

### Coverage Impact Estimate
| File | Before | After (est.) | New Tests |
|------|--------|--------------|-----------|
| main.rs | 0% | ~8-12% | ~20 tests |
| backup/mod.rs | 46.41% | ~48-50% | ~12 tests |
| download/mod.rs | 44.27% | ~46-48% | ~8 tests |
| restore/attach.rs | 45.95% | ~48-50% | ~12 tests |
| Overall | 66.68% | ~68-70% | ~52 tests |

### Anti-Overengineering
- No test helpers, no test fixtures, no abstractions. Each test is a simple function with arrange/act/assert.
- No mocking framework introduction.
- Tests follow the exact same pattern as existing tests in the codebase.
