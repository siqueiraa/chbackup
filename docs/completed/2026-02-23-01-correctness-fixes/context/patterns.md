# Pattern Discovery

## Global Patterns Registry
No `docs/patterns/` directory exists. Full local discovery performed.

## Pattern 1: URL Encoding Functions (DRY Violation - Issue 7)

Four independent implementations of essentially the same logic:

| Module | Function | Visibility | Preserves `/` | Implementation Style |
|--------|----------|------------|---------------|---------------------|
| `src/backup/collect.rs:29` | `url_encode_path(s)` | `pub` | YES | `String::with_capacity` + byte-level percent-encode |
| `src/download/mod.rs:41` | `url_encode(s)` | `fn` (private) | YES | `.chars().map().collect()` |
| `src/upload/mod.rs:55` | `url_encode_component(s)` | `fn` (private) | NO | `.chars().map().collect()` |
| `src/restore/attach.rs:844` | `url_encode(s)` | `pub(crate)` | YES | `.chars().map().collect()` |

**Key difference**: `upload::url_encode_component` does NOT preserve `/` because it encodes individual path components. The other three preserve `/` because they handle full paths (but this is actually a subtle bug: db/table names should NOT contain `/`).

**Correct approach**: Two canonical functions:
1. `encode_path_component(s)` -- for individual db or table names (NO `/` preserved)
2. Use component encoder and join with `/` where needed

## Pattern 2: Config CLI Override (set_field pattern)

`Config::set_field(key, value)` uses a flat `match` on dot-notation keys (e.g., `"s3.bucket"`, `"clickhouse.host"`). 200+ match arms. Unknown keys return `Err`.

The `apply_env_overlay()` uses a separate hand-coded mapping with uppercase env-var names (`S3_BUCKET`, `CLICKHOUSE_HOST`).

**Issue 6 gap**: `apply_cli_env_overrides()` calls `set_field()` which only accepts dot-notation. Design doc says `--env S3_BUCKET=other-bucket` should work.

## Pattern 3: check_parts_columns Behavior

Current flow (src/backup/mod.rs:192-226):
1. If `config.clickhouse.check_parts_columns && !skip_check_parts_columns`:
2. Query `check_parts_columns(targets)`
3. Filter benign drift
4. If actionable: log warnings + `info!("proceeding anyway")`
5. **Never fails** -- always continues to FREEZE

## Pattern 4: S3 Client Construction

`S3Client::new(config)` at s3.rs:64:
- Uses `aws_config::from_env()` for SDK config
- Uses `aws_sdk_s3::config::Builder::from(&sdk_config)` for S3-specific config
- `disable_cert_verification` currently sets `AWS_CA_BUNDLE=""` (process-global env var) -- broken
- `disable_ssl` field exists in config but is never read in `S3Client::new()`

## Pattern 5: S3 Unit Test Mock Pattern

`mock_s3_client()` at s3.rs:1520:
- Creates `aws_sdk_s3::config::Builder::new()` with `.behavior_version_latest()`
- Sets region
- Builds `aws_sdk_s3::Client::from_conf(s3_config)`
- This triggers TLS initialization (rustls) which requires network/cert stores
- Used by 8 tests, 3 of which are `#[tokio::test] async` (make actual S3 calls that fail)
- The 5 sync tests (`test_full_key_*`, `test_multipart_chunk_*`, `test_copy_object_builds_*`) could use a simpler mock
