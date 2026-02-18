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
- `inner() -> &aws_sdk_s3::Client` -- Access underlying client
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

### CopyObject API (Phase 2c)
- `copy_object(source_bucket, source_key, dest_key) -> Result<()>` -- Server-side copy using AWS SDK `CopyObject`. CopySource format: `"{source_bucket}/{source_key}"`. Applies SSE and storage_class settings. Destination key is relative to self's prefix.
- `copy_object_streaming(source_bucket, source_key, dest_key) -> Result<()>` -- Fallback for cross-region copy failures. Downloads from source via raw AWS SDK client (`self.inner`), then uploads to dest via `self.put_object()`. Higher network cost but works across regions.
- `copy_object_with_retry(source_bucket, source_key, dest_key, allow_streaming) -> Result<()>` -- Retry wrapper: retries `copy_object()` up to 3 times with exponential backoff (100ms, 400ms, 1600ms). On final failure: if `allow_streaming=true`, falls back to `copy_object_streaming()` with `warn!()` about high network traffic; if `false`, returns the error.
- Used by upload (Task 6) and restore (Task 8) for S3 disk parts.

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
