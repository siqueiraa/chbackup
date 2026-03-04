# Pattern Discovery

## Global Patterns

No global patterns directory exists (`docs/patterns/` not found). All patterns discovered locally from existing test modules.

## Test Patterns in chbackup

### Pattern 1: Inline #[cfg(test)] Module (used everywhere)

Every source file with tests uses an inline `#[cfg(test)] mod tests` block at the bottom of the file.

**Reference implementations:**
- `src/backup/mod.rs` (lines ~909+): Tests for FreezeInfo, TableRow, partition SQL
- `src/upload/mod.rs`: Tests for should_use_multipart, s3_key_for_part, find_part_dir
- `src/download/mod.rs`: Tests for work items, disk space, hardlink dedup
- `src/server/routes.rs`: Extensive serde tests, paginate, format_duration

**Structure:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    // additional imports as needed

    #[test]
    fn test_function_name() {
        // arrange
        // act
        // assert
    }
}
```

### Pattern 2: tempdir-Based Filesystem Tests

Used when testing functions that create/read files or directories.

**Reference:** `src/upload/mod.rs` tests for `find_part_dir` and `collect_files_recursive`:
```rust
#[test]
fn test_find_part_dir_per_disk() {
    let tmp = tempfile::tempdir().unwrap();
    // create directory structure
    std::fs::create_dir_all(tmp.path().join("...")).unwrap();
    // call function
    // assert result
}
```

### Pattern 3: Serde Round-Trip Tests

Used for request/response types in server routes.

**Reference:** `src/server/routes.rs` - tests for ListResponse, BackupRequest, etc.:
```rust
#[test]
fn test_list_response_serialization() {
    let resp = ListResponse { ... };
    let json = serde_json::to_string(&resp).unwrap();
    let parsed: ListResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(resp.name, parsed.name);
}
```

### Pattern 4: Pure Function Tests (dominant pattern)

Most unit tests are simple input->output tests on pure functions.

**Reference:** `src/storage/s3.rs` tests for `calculate_chunk_size`, `percent_encode_s3_key`:
```rust
#[test]
fn test_calculate_chunk_size_auto() {
    let result = calculate_chunk_size(100_000_000, 0, 10000);
    assert!(result >= 5 * 1024 * 1024); // 5 MiB minimum
}
```

### Pattern 5: Error Case Tests

Tests verifying error conditions return appropriate errors.

**Reference:** `src/server/routes.rs` tests for validation_error, paginate edge cases.

## Test Infrastructure Notes

- No mocking framework is used; tests rely on pure functions or tempfile-based filesystem
- `mock_s3_fields()` in `storage/s3.rs` constructs a dummy S3Client for offline unit tests
- `ChClient` is not mockable for unit tests; all ChClient-dependent code requires integration tests
- `#[ignore]` used for tests requiring network/S3 connections
- `tracing` subscriber can only be initialized once per process; logging tests are limited
