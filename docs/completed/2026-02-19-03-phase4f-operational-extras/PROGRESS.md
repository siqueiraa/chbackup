# Progress: Phase 4f -- Operational Extras

## Completed Tasks

### Task 2: Add check_json_columns() to ChClient
- **Status:** PASS
- **Commit:** ab3c364e
- **Files:** src/clickhouse/client.rs, src/clickhouse/mod.rs
- **Acceptance:** F002
- **Summary:** Added `JsonColumnInfo` struct and `check_json_columns()` method to ChClient. Follows the exact pattern of `check_parts_columns()` -- queries `system.columns` for columns with Object or JSON types. Added unit tests for struct and SQL pattern verification. Exported `JsonColumnInfo` from clickhouse module.

### Task 3: Integrate JSON column check into backup pre-flight
- **Status:** PASS
- **Commit:** 210ba7a0
- **Files:** src/backup/mod.rs
- **Acceptance:** F002
- **Summary:** Added JSON/Object column type detection (step 5c) in backup pre-flight, right after the existing parts column consistency check. Uses the same try/match pattern. Warning-only -- never blocks backup. Logs each detected JSON column and a summary count.

### Task 5: Add zstd and flate2 crate dependencies
- **Status:** PASS
- **Commit:** 2e04d783
- **Files:** Cargo.toml
- **Acceptance:** F004
- **Summary:** Added `flate2 = "1"` and `zstd = "0.13"` crate dependencies to Cargo.toml alongside existing `lz4_flex`. Both crates resolve and compile cleanly.

### Task 6: Add multi-format compress_part() and decompress_part()
- **Status:** PASS
- **Commit:** 01f96c75
- **Files:** src/upload/stream.rs, src/download/stream.rs
- **Acceptance:** F004
- **Summary:** Extended `compress_part()` in both upload and download stream modules to accept `data_format: &str` and `compression_level: u32` parameters. Extended `decompress_part()` in download/stream.rs to accept `data_format: &str`. Added `archive_extension()` helper for consistent extension mapping. Supports lz4, zstd, gzip, and none formats. Added comprehensive roundtrip tests for all 4 formats plus unknown format error tests.

### Task 7: Wire format through upload and download pipelines
- **Status:** PASS
- **Commit:** 4a8474b4
- **Files:** src/upload/mod.rs, src/download/mod.rs
- **Acceptance:** F004
- **Summary:** Updated `s3_key_for_part()` to accept `data_format` parameter and use dynamic extension via `archive_extension()`. Updated `compress_part` call site in upload to pass `data_format` and `compression_level` from config. Updated `decompress_part` call site in download to pass `manifest.data_format`. Added test for format-specific S3 key generation. Updated all existing test call sites.

## Pending Tasks

- Task 4: Implement tables command (Group C)
- Task 8: Update CLAUDE.md (Group E)
