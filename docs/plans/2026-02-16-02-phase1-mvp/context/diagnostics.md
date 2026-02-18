# Diagnostics — Phase 1 MVP

## Compiler State

**Timestamp:** 2026-02-16
**Git commit:** 880a640 (master)
**Command:** `cargo check`

### Result: CLEAN (0 errors, 0 warnings)

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.58s
```

## Test State

**Command:** `cargo test`

### Result: ALL PASS (14 tests, 0 failures)

```
Unit tests (src/lib.rs):
  - config::tests::test_parse_duration_secs .................. ok
  - clickhouse::client::tests::test_ch_client_new_default_config ok
  - clickhouse::client::tests::test_ch_client_new_secure ..... ok
  - clickhouse::client::tests::test_ch_client_new_with_credentials ok
  - lock::tests::test_acquire_release ....................... ok
  - lock::tests::test_double_acquire_fails .................. ok
  - lock::tests::test_stale_lock_overridden ................. ok
  - lock::tests::test_lock_for_command_mapping .............. ok
  - storage::s3::tests::test_s3_config_defaults ............. ok

Integration tests (tests/config_test.rs):
  - test_default_config_serializes .......................... ok
  - test_cli_env_override ................................... ok
  - test_config_from_yaml ................................... ok
  - test_env_overlay ........................................ ok
  - test_validation_full_interval ........................... ok
```

## Dependency Analysis

### Current Cargo.toml Dependencies

| Crate | Version | Used By |
|-------|---------|---------|
| clap | 4 (derive, env) | CLI parsing |
| serde | 1 (derive) | Config serialization |
| serde_yaml | 0.9 | Config file format |
| serde_json | 1 | Lock file, future manifest |
| thiserror | 2 | Error types |
| anyhow | 1 | Error propagation |
| tokio | 1 (full) | Async runtime |
| tokio-util | 0.7 (codec) | Streaming utilities |
| tracing | 0.1 | Logging |
| tracing-subscriber | 0.3 (json, env-filter) | Log formatting |
| clickhouse | 0.13 (inserter) | ClickHouse HTTP client |
| aws-sdk-s3 | 1 | S3 operations |
| aws-config | 1 (behavior-version-latest) | AWS SDK configuration |
| lz4_flex | 0.11 | LZ4 compression (already present, not yet used) |
| walkdir | 2 | Directory traversal (already present, not yet used) |
| chrono | 0.4 (serde) | Timestamps |
| libc | 0.2 | PID liveness check |
| tempfile | 3 (dev) | Test temp directories |

### Dependencies to Add for Phase 1

| Crate | Version | Purpose | Verification |
|-------|---------|---------|-------------|
| glob | 0.3 | Table filter pattern matching (-t flag) | crates.io verified |
| nix | 0.29 (features: fs, user) | chown for restored files, stat for detecting ClickHouse uid/gid | crates.io verified |
| crc64fast | 1 | CRC64 checksum computation | NEEDS VERIFICATION on crates.io; alternative: `crc` crate with CRC-64/XZ algorithm |
| tar | 0.4 | Archive creation/extraction for part directories | crates.io verified |
| async-compression | 0.4 (features: tokio, lz4) | Async streaming LZ4 compress/decompress | crates.io verified; may replace direct lz4_flex usage for streaming |

### Important: lz4_flex vs async-compression Decision

- `lz4_flex` (already in Cargo.toml) is synchronous frame encoder/decoder
- `async-compression` provides AsyncRead/AsyncWrite wrappers around lz4
- For streaming upload/download pipeline, `async-compression` is needed
- `lz4_flex` may still be useful for non-streaming operations
- Design doc specifies streaming pipeline: `AsyncRead(file) -> tar_stream -> lz4_encoder -> S3 PutObject body`

## Existing Code Issues (None)

No pre-existing compiler warnings or errors. Codebase is clean as of Phase 0 completion.

## Risks for Phase 1

1. **clickhouse crate Row derive**: Need to verify that `#[derive(clickhouse::Row, serde::Deserialize)]` works for system table queries (system.tables, system.mutations, system.parts). The clickhouse crate uses HTTP interface and may have specific requirements for column mapping.

2. **aws-sdk-s3 ByteStream from AsyncRead**: Need to verify how to wrap an async reader (compression pipeline output) as a ByteStream for PutObject. The SDK v1 API may require specific conversion.

3. **Cross-device hardlink (EXDEV)**: `std::fs::hard_link` returns `io::Error` with `ErrorKind::CrossesDevices` on EXDEV. Need to verify this ErrorKind exists in stable Rust (it was stabilized in Rust 1.74).

4. **CRC64 crate selection**: `crc64fast` crate existence on crates.io needs verification. The `crc` crate (v3) supports CRC-64/XZ algorithm and is well-maintained. Need to match the exact CRC64 variant used by ClickHouse's checksums.txt.
