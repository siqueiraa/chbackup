# Data Authority Analysis

This plan modifies existing code behavior; it does NOT add new tracking, accumulators, or calculations.

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Path component sanitization | db/table names from manifest | `String` (table_key) | MUST IMPLEMENT - no existing sanitizer strips `..` or leading slashes |
| TLS config for S3 | `S3Config.disable_cert_verification` | `bool` | MUST IMPLEMENT - current impl is broken (sets process-global env var) |
| Column inconsistency data | `ChClient.check_parts_columns()` | `Vec<ColumnInconsistency>` | USE EXISTING - data source already provides what we need |
| Config key mapping | `apply_env_overlay()` env var names | hardcoded mapping | MUST IMPLEMENT - translation from `S3_BUCKET` to `s3.bucket` format |

## Analysis Notes

- **Issue 1** (path traversal): The `url_encode` functions are the data authority for path encoding. They need a sanitization wrapper, not a replacement. The trust model is "trusted-only sanitize" -- strip leading slashes, block `..` components.
- **Issue 2** (disable_cert_verification): The AWS SDK for Rust uses rustls. The current `std::env::set_var("AWS_CA_BUNDLE", "")` approach is broken -- it's a process-global env var that (a) doesn't disable cert verification and (b) is unsafe in multi-threaded code. The SDK has NO public "skip cert verification" API. The fix requires building a custom `rustls::ClientConfig` with `dangerous().set_certificate_verifier(NoVerifier)`, wrapping it in a `hyper-rustls` connector, and using the `hyper_014::HyperClientBuilder::build(custom_connector)` API. All required crate features (`hyper-014`, `legacy-rustls-ring`, `rustls-aws-lc`) are already enabled in the dependency tree.
- **Issue 3** (S3 unit tests): `mock_s3_client()` builds a real `aws_sdk_s3::Client::from_conf()` which initializes rustls. The 5 sync tests only need `full_key()` and `calculate_chunk_size()` -- they don't need a real client at all. The 3 async tests intentionally exercise real S3 calls (and expect failures), but the TLS init still requires access to system cert stores (fails with `--locked --offline`).
- **Issue 4** (disable_ssl): The `S3Config.disable_ssl` field is parsed from config/env/CLI but never read in `S3Client::new()`. Per design doc section 12, `disable_ssl: true` means "use HTTP instead of HTTPS". The fix is to prefix the endpoint with `http://` when `disable_ssl` is true.
- **Issue 5** (check_parts_columns): The `check_parts_columns()` query already returns all needed data. The issue is the control flow (warn vs fail), not the data.
- **Issue 6** (--env format): The env var name -> dot-notation key mapping already exists in `apply_env_overlay()` as hardcoded `std::env::var()` calls. We need a reverse lookup table.

## Decisions Summary

- **USE EXISTING**: 1 (column inconsistency data)
- **MUST IMPLEMENT**: 3 (path sanitizer, TLS config, env-key translator)
- No over-engineering flags raised
