# CLAUDE.md -- src/storage

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module provides the `S3Client` wrapper around `aws-sdk-s3`. It centralizes all S3 interactions: object upload/download, listing, deletion, and metadata queries. Supports custom endpoints (MinIO, R2), SSE encryption, and storage class configuration.

## Directory Structure

```
src/storage/
  mod.rs    -- Re-exports S3Client and S3Object
  s3.rs     -- S3Client struct with all S3 operation methods
```

## Key Patterns

### Client Wrapper Pattern
`S3Client` wraps `aws_sdk_s3::Client` with:
- Config-driven construction from `S3Config` (region, bucket, endpoint, credentials, path_style)
- Key prefix management via `full_key()` helper
- Storage class, SSE, and KMS key ID applied automatically to uploads
- Support for custom endpoints (force_path_style for MinIO/R2)
- Static credentials or default AWS credential chain
- Optional `assume_role_arn` for cross-account access

### TLS / SSL Configuration (s3.rs)
- **`disable_ssl=true`**: Rewrites `https://` endpoint to `http://` via `effective_endpoint` in `S3Client::new()`. When endpoint is empty (default AWS), logs a warning and continues (user may set endpoint via env var).
- **`disable_cert_verification=true`**: Forces the endpoint scheme to HTTP (the broken `std::env::set_var("AWS_CA_BUNDLE", "")` approach has been removed). Requires an explicit endpoint URL -- bails with error if endpoint is empty. The AWS SDK for Rust (aws-smithy-http-client v1.1.10) has no public API for TLS certificate skip; HTTP fallback is the pragmatic solution.
- Both flags compute an `effective_endpoint` that is used for the SDK loader and config builder. If both are true, the rewrite is idempotent (only one `https://` -> `http://` replacement).

### Hermetic Test Helper (s3.rs, test module)
- `mock_s3_fields(bucket, prefix) -> S3Client` constructs an `S3Client` with a dummy inner client (no TLS native-root initialization) for use in sync unit tests. Safe for `cargo test --locked --offline`.
- The 3 async retry tests (`test_copy_object_with_retry_no_streaming_when_disabled`, `test_put_object_retry_config`, `test_upload_part_retry_config`) are marked `#[ignore]` as they require network/S3 error paths.

### S3Object Type
```rust
pub struct S3Object {
    pub key: String,
    pub size: i64,
    pub last_modified: Option<DateTime<Utc>>,
}
```

### Key Prefix Handling
`full_key(relative_key)` prepends the configured prefix with proper slash handling:
- Empty prefix: returns key as-is
- Prefix without trailing slash: adds one
- Ensures no double slashes

### Public API
- `new(config) -> Result<Self>` -- Build from S3Config (async)
- `ping() -> Result<()>` -- Connectivity check via HeadBucket
- `bucket() -> &str` -- Configured bucket name
- `prefix() -> &str` -- Configured key prefix
- `full_key(relative_key) -> String` -- Prepend prefix to key
- `put_object(key, body) -> Result<()>` -- Upload with default options
- `put_object_with_options(key, body, content_type) -> Result<()>` -- Upload with SSE/storage class
- `get_object(key) -> Result<Vec<u8>>` -- Download full object to memory
- `get_object_stream(key) -> Result<ByteStream>` -- Download as streaming body
- `list_common_prefixes(prefix, delimiter) -> Result<Vec<String>>` -- List "directory" prefixes
- `list_objects(prefix) -> Result<Vec<S3Object>>` -- List all objects (handles pagination >1000)
- `delete_object(key) -> Result<()>` -- Delete single object
- `delete_objects(keys) -> Result<()>` -- Batch delete (groups of 1000 per S3 API limit)
- `head_object(key) -> Result<Option<u64>>` -- Check existence and get size
- `put_object_with_retry(key, body, retry) -> Result<()>` -- PutObject with retry/backoff/jitter (Phase 7)
- `upload_part_with_retry(key, upload_id, part_number, body, retry) -> Result<String>` -- UploadPart with retry (Phase 7)
- `copy_object(source_bucket, source_key, dest_key) -> Result<()>` -- Server-side copy with SSE/storage_class (Phase 2c)
- `copy_object_streaming(source_bucket, source_key, dest_key) -> Result<()>` -- Download+upload fallback for cross-region (Phase 2c)
- `copy_object_with_retry(source_bucket, source_key, dest_key, allow_streaming) -> Result<()>` -- Retry wrapper with exponential backoff and conditional streaming fallback (Phase 2c)
- `create_multipart_upload(key) -> Result<String>` -- Initiate multipart upload
- `upload_part(key, upload_id, part_number, body) -> Result<String>` -- Upload chunk
- `complete_multipart_upload(key, upload_id, parts) -> Result<()>` -- Finalize multipart
- `abort_multipart_upload(key, upload_id) -> Result<()>` -- Cancel multipart
- `calculate_chunk_size(data_len, config_chunk_size, max_parts_count) -> u64` -- Chunk sizing (standalone fn)

### Multipart Upload API (Phase 2a)
- `create_multipart_upload(key) -> Result<String>` -- Initiate multipart upload, returns upload_id. Applies same SSE/storage_class as `put_object`.
- `upload_part(key, upload_id, part_number, body) -> Result<String>` -- Upload a single chunk, returns ETag. Part numbers must be 1-10000.
- `complete_multipart_upload(key, upload_id, parts) -> Result<()>` -- Finalize with list of `(part_number, e_tag)` tuples.
- `abort_multipart_upload(key, upload_id) -> Result<()>` -- Cancel and clean up partial uploads.
- `calculate_chunk_size(data_len, config_chunk_size, max_parts_count) -> u64` -- Standalone pure function. When `config_chunk_size` is 0, auto-computes from `data_len / max_parts_count`. Enforces 5 MiB minimum (S3 requirement).

### RetryConfig Type
```rust
pub struct RetryConfig {
    pub max_retries: u32,        // 0 = no retries, single attempt
    pub base_delay_secs: u64,    // exponentially increases per attempt
    pub jitter_factor: f64,      // 0.0-1.0, applied via config::apply_jitter()
}
```
Constructed from `crate::config::effective_retries()`. Shared across `put_object_with_retry()`, `upload_part_with_retry()`, and `copy_object_with_retry_jitter()`.

### PutObject/UploadPart Retry (Phase 7)
- `put_object_with_retry(key, body, retry) -> Result<()>` -- Retries `put_object()` up to `retry.max_retries` times with exponential backoff and configurable jitter. Clones the body for each retry attempt. On final failure, returns error with attempt count and full key in context.
- `upload_part_with_retry(key, upload_id, part_number, body, retry) -> Result<String>` -- Retries `upload_part()` with the same exponential backoff pattern. Returns the ETag on success.
- Both methods use `config::apply_jitter()` for delay randomization and `tokio::time::sleep` for async-safe waiting.
- Wired into the upload pipeline (`src/upload/mod.rs`) where `put_object()` and `upload_part()` were previously called directly.

### CopyObject API (Phase 2c)
- `copy_object(source_bucket, source_key, dest_key) -> Result<()>` -- Server-side copy using AWS SDK `CopyObject`. CopySource format: `"{source_bucket}/{source_key}"`. Applies SSE and storage_class settings. Destination key is relative to self's prefix.
- `copy_object_streaming(source_bucket, source_key, dest_key) -> Result<()>` -- Fallback for cross-region copy failures. Downloads from source via raw AWS SDK client (`self.inner`), then uploads to dest via `self.put_object()`. Higher network cost but works across regions.
- `copy_object_with_retry(source_bucket, source_key, dest_key, allow_streaming) -> Result<()>` -- Retry wrapper: retries `copy_object()` up to 3 times with exponential backoff (100ms, 400ms, 1600ms). On final failure: if `allow_streaming=true`, falls back to `copy_object_streaming()` with `warn!()` about high network traffic; if `false`, returns the error.
- Used by upload (Task 6) and restore (Task 8) for S3 disk parts.

### Manifest Atomicity Support (Phase 2d)
- Upload module uses existing `copy_object()` and `delete_object()` for atomic manifest upload:
  1. Upload manifest to `{backup_name}/metadata.json.tmp`
  2. `copy_object(bucket, tmp_key, final_key)` to atomically make it visible
  3. `delete_object(tmp_key)` to clean up
- No new S3Client methods needed; existing API is sufficient

### Error Handling
- All methods return `anyhow::Result` with `.context()` annotations
- `list_objects` handles continuation tokens for pagination automatically
- `delete_objects` batches in groups of 1000 (S3 API limit)
- `head_object` returns `None` for 404 (object not found)

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real S3
