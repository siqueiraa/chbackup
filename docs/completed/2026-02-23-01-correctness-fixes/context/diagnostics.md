# Diagnostics Report

## Compiler State

**Command**: `cargo check`
**Result**: SUCCESS -- zero errors, zero warnings
**Timestamp**: 2026-02-23

## Current Baseline

The codebase compiles cleanly. No pre-existing errors or warnings to account for.

## Clippy Notes

Not run during this analysis (clean `cargo check` is sufficient for plan analysis).
Implementation phase should run `cargo clippy` before committing.

## Key Observations for Plan Issues

### Issue 1: Path Traversal
- No compiler-level path validation exists. The `url_encode` functions are pure string transforms with no sanitization.
- `url_encode` in download (line 41) preserves `/`, meaning a db/table name containing `../` would produce a valid but traversal-vulnerable path component.
- Risk: trusted-only (manifest data comes from ClickHouse, not user input), but defense-in-depth warrants sanitization.

### Issue 2: disable_cert_verification
- The current `std::env::set_var("AWS_CA_BUNDLE", "")` at s3.rs:161 compiles fine but is:
  - (a) Process-global (unsafe in multi-threaded async code)
  - (b) Does NOT actually disable cert verification -- `AWS_CA_BUNDLE=""` is not a valid rustls flag
- AWS SDK Rust 1.x uses `aws-smithy-runtime` which internally uses `hyper-rustls`. The dependency tree shows:
  - `hyper-rustls v0.24.2` (uses `rustls v0.21.12`) -- legacy connector
  - `hyper-rustls v0.27.7` (uses `rustls v0.23.36`) -- modern connector
- The `aws-sdk-s3::config::Builder` does NOT expose a `http_client()` setter in the public API per the v1.123.0 docs. The customization point is via `aws-smithy-runtime::client::http::hyper_014::HyperClientBuilder` (if the `hyper-014` feature is enabled).

### Issue 3: S3 Unit Tests / TLS Init
- `mock_s3_client()` at s3.rs:1520 creates `aws_sdk_s3::Client::from_conf(s3_config)` which initializes rustls.
- This happens even for tests that only call `full_key()` or `calculate_chunk_size()`.
- The 5 sync tests (`test_full_key_*`, `test_multipart_chunk_*`, `test_copy_object_builds_*`) only need bucket/prefix fields.
- The 3 async tests (`test_copy_object_with_retry_*`, `test_put_object_retry_*`, `test_upload_part_retry_*`) make actual S3 calls that fail (intentionally testing error paths).
- `mock_s3_client` is called 8 times in tests (verified via grep and LSP references: lines 1338, 1347, 1356, 1365, 1430, 1448, 1470, 1496).

### Issue 4: disable_ssl
- `S3Config.disable_ssl` (config.rs:280) is parsed from YAML, env vars, and CLI overrides.
- It is NEVER read in `S3Client::new()` -- the field is completely dead after config loading.
- The `S3Client::new()` code at s3.rs:77 uses `config.endpoint` as-is (could be http:// or https://).

### Issue 5: check_parts_columns
- Current behavior at backup/mod.rs:192-226: when `check_parts_columns=true && !skip_check_parts_columns`:
  - Queries ClickHouse
  - Filters benign drift
  - Logs warnings + `info!("proceeding anyway")`
  - **Never returns an error** -- backup always proceeds
- `filter_benign_type_drift()` has 2 incoming callers: `create()` and `test_parts_columns_check_skip_benign_types`
- `ColumnInconsistency` fields verified: `database: String`, `table: String`, `column: String`, `types: Vec<String>`

### Issue 6: --env format
- `apply_cli_env_overrides()` at config.rs:1100 calls `set_field(key.trim(), value.trim())`
- `set_field()` has ~80 match arms, ALL using dot-notation keys (e.g., `"s3.bucket"`, `"clickhouse.host"`)
- `apply_env_overlay()` at config.rs:871 uses uppercase env var names (e.g., `S3_BUCKET`, `CLICKHOUSE_HOST`)
- There is NO translation layer between the two formats
- Users passing `--env S3_BUCKET=other-bucket` get `Unknown config key: 'S3_BUCKET'`

### Issue 7: DRY url_encode
- 4 independent implementations verified via grep and LSP:
  - `backup::collect::url_encode_path()` -- pub, preserves `/`, byte-level encoding
  - `download::url_encode()` -- private, preserves `/`, char-level encoding
  - `upload::url_encode_component()` -- private, does NOT preserve `/`, char-level encoding
  - `restore::attach::url_encode()` -- pub(crate), preserves `/`, char-level encoding
- All 4 have tests. Combined 30+ call sites across the codebase.
- The implementations differ in:
  - `/` handling (upload does not preserve it)
  - Encoding method (collect uses byte-level for multi-byte chars, others use `c as u32`)
  - The byte-level vs char-level difference is a subtle correctness issue for multi-byte UTF-8
