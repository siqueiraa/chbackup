# S3 Storage Operations: Go vs Rust Parity Analysis

**Date**: 2026-02-20
**Go source**: `pkg/storage/s3.go` (975 lines), `pkg/storage/general.go` (727 lines), `pkg/storage/object_disk/object_disk.go` (771 lines), `pkg/storage/structs.go` (91 lines), `pkg/storage/utils.go` (123 lines)
**Rust source**: `src/storage/s3.rs` (1395 lines), `src/storage/mod.rs` (3 lines), `src/object_disk.rs` (579 lines)

---

## 1. S3 Client Initialization & Credentials

### Go Implementation (`S3.Connect()`, lines 118-189)
- Loads default AWS SDK v2 config with `RetryModeStandard`
- Region from config
- **IRSA handling**: Reads `AWS_ROLE_ARN` and `AWS_WEB_IDENTITY_TOKEN_FILE` env vars. If both present, uses `stscreds.NewWebIdentityRoleProvider`. Then chains with `AssumeRoleARN` if it differs from IRSA role
- **AssumeRole priority**: `S3_ASSUME_ROLE_ARN` > `AWS_ROLE_ARN` (issue #898). Uses `stscreds.NewAssumeRoleProvider` which auto-refreshes credentials
- **Static credentials**: Override everything if `AccessKey` + `SecretKey` are provided (last in chain, highest priority)
- **Debug logging**: When `Debug=true`, sets `aws.LogRetries | aws.LogRequest | aws.LogResponse` with zerolog adapter
- **TLS**: Custom `http.Transport` with `InsecureSkipVerify` when `DisableCertVerification=true`
- **GCS compatibility**: Custom `RecalculateV4Signature` round-tripper that removes `Accept-Encoding` from signature when endpoint contains `storage.googleapis.com`
- **Endpoint resolution**: Custom `ResolveEndpoint` method on S3 struct. Sets `ForcePathStyle` and custom endpoint via `EndpointResolverV2`
- **DisableSSL**: `o.EndpointOptions.DisableHTTPS = s.Config.DisableSSL`
- **Versioning detection**: Calls `GetBucketVersioning` at connect time, stores result

### Rust Implementation (`S3Client::new()`, lines 53-175)
- Loads default AWS config from env with region
- Custom endpoint if non-empty
- Static credentials if access_key + secret_key provided
- AssumeRole via one-shot `sts_client.assume_role()` call (not auto-refreshing)
- `force_path_style` on S3 config builder
- `disable_cert_verification` sets `AWS_CA_BUNDLE=""` env var (documented as workaround)
- Debug mode just logs a message, no SDK log level change

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 1.1 | **IRSA (Web Identity Token) support missing** | MEDIUM | Go handles `AWS_ROLE_ARN` + `AWS_WEB_IDENTITY_TOKEN_FILE` via `stscreds.NewWebIdentityRoleProvider`. Rust relies on SDK default chain which may handle this, but the explicit priority logic (IRSA -> AssumeRole chaining) is not replicated. Critical for EKS/K8s. |
| 1.2 | **AssumeRole credential auto-refresh missing** | MEDIUM | Go uses `stscreds.NewAssumeRoleProvider` which auto-refreshes before expiry. Rust does a one-shot `assume_role()` call and stores static temporary credentials. Long-running server/watch mode will fail after STS token expires (typically 1h). |
| 1.3 | **AssumeRole priority chain incomplete** | LOW | Go has 4-way priority: (1) static creds, (2) config AssumeRoleARN, (3) env AWS_ROLE_ARN, (4) IRSA. With static creds overriding everything. Rust only handles static creds and config AssumeRoleARN. The env `AWS_ROLE_ARN` without IRSA is not explicitly handled (though SDK default chain may cover it). |
| 1.4 | **SDK retry mode not configured** | LOW | Go explicitly sets `RetryModeStandard`. Rust uses SDK defaults. The AWS SDK Rust likely defaults to standard retry already, but it is not explicitly configured. |
| 1.5 | **Debug SDK logging not functional** | LOW | Go wires `aws.LogRetries | aws.LogRequest | aws.LogResponse` to zerolog. Rust just logs "debug mode enabled" but does not enable SDK-level request/response logging. |
| 1.6 | **disable_cert_verification is a workaround** | LOW | Rust sets `AWS_CA_BUNDLE=""` which is a side-effect-based hack. Go properly creates a custom `http.Transport` with `InsecureSkipVerify`. The Rust approach may not work correctly since empty `AWS_CA_BUNDLE` might cause SDK to use system CA bundle rather than skip verification. |
| 1.7 | **GCS-over-S3 signature compatibility missing** | LOW | Go has `RecalculateV4Signature` round-tripper for `storage.googleapis.com` endpoints. Rust has no GCS compatibility. Not required for S3-only scope, but Go supports it. |
| 1.8 | **DisableSSL (HTTP endpoints) not wired** | LOW | Go sets `EndpointOptions.DisableHTTPS`. Rust has `disable_ssl` in config but does not use it. If users provide `http://` in the endpoint URL, the SDK may handle it, but explicit disable is missing. |
| 1.9 | **Bucket versioning not detected** | LOW | Go calls `GetBucketVersioning` at connect time and handles versioned deletes. Rust has no versioning awareness. |

---

## 2. Put/Upload Operations

### Go Implementation (`S3.PutFile` / `S3.PutFileAbsolute`, lines 286-350)
- Uses `s3manager.NewUploader` (high-level SDK manager) with configurable `Concurrency` and `BufferSize`
- **Chunk size calculation**: If `ChunkSize > 0` and fits in `MaxPartsCount`, use it. Otherwise auto-calculates from `localSize / MaxPartsCount` with remainder handling
- **AdjustValueByRange**: Clamps part size to [5 MiB, 5 GiB]
- **CheckSumAlgorithm**: Optional checksum on upload (CRC32, CRC32C, SHA1, SHA256)
- **Object labels/tags**: `Tagging` header from `ObjectLabels` map
- **SSE**: Supports `SSE`, `SSEKMSKeyId`, `SSECustomerAlgorithm`, `SSECustomerKey`, `SSECustomerKeyMD5`, `SSEKMSEncryptionContext` (6 SSE fields)
- **ACL**: Optional, only set when non-empty (issue #785)
- **RequestPayer**: For requester-pays buckets
- **Storage class**: Uppercased

### Rust Implementation (`S3Client::put_object`, lines 251-313; multipart lines 559-753)
- Simple `put_object` for small objects (buffered in memory)
- Manual multipart upload with `create_multipart_upload` / `upload_part` / `complete_multipart_upload`
- SSE: Only `SSE` (AES256/aws:kms) and `SSEKMSKeyId` (2 of 6 fields)
- ACL: Applied when non-empty
- Storage class: Uppercased
- No object tagging/labels
- No checksum algorithm
- No request payer
- No `s3manager`-style built-in concurrency (manual multipart instead)

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 2.1 | **SSE-C (customer-provided key) not supported** | MEDIUM | Go supports `SSECustomerAlgorithm`, `SSECustomerKey`, `SSECustomerKeyMD5` on all operations (put, get, head, copy, multipart). Rust only supports server-managed SSE (AES256, aws:kms). SSE-C is used by some organizations for compliance. |
| 2.2 | **SSE KMS encryption context not supported** | LOW | Go supports `SSEKMSEncryptionContext` for additional KMS context. Rust does not. |
| 2.3 | **Object tagging/labels not supported** | MEDIUM | Go supports `ObjectLabels` map applied as S3 tagging on Put, Copy, and CreateMultipartUpload. Used for cost allocation, lifecycle policies, etc. (issue #588). |
| 2.4 | **ChecksumAlgorithm not supported** | LOW | Go supports optional S3 checksum algorithms (CRC32, CRC32C, SHA1, SHA256). Rust does not set this. |
| 2.5 | **RequestPayer not supported** | LOW | Go supports requester-pays buckets. Rust does not. |
| 2.6 | **Part size upper bound not enforced** | LOW | Go clamps part size to max 5 GiB via `AdjustValueByRange`. Rust only enforces the 5 MiB minimum. Extremely large chunk sizes could exceed the S3 5 GiB part limit. |
| 2.7 | **Multipart download not supported** | LOW | Go has `AllowMultipartDownload` using `s3manager.NewDownloader` for parallel range-based downloads. Rust downloads full objects sequentially. |

---

## 3. Get/Download Operations

### Go Implementation (`S3.GetFileReader*`, lines 195-284)
- Supports SSE-C parameters on GetObject
- **Glacier restore**: Detects `InvalidObjectState` with GLACIER storage class, calls `RestoreObject` with Expedited tier, polls with exponential backoff until restored
- **Multipart download**: `s3manager.NewDownloader` with `Concurrency` and configurable part size when `AllowMultipartDownload=true`
- **RequestPayer** on get operations

### Rust Implementation (`S3Client::get_object`, lines 320-363)
- Simple `get_object` collecting full body into memory
- `get_object_stream` returning `ByteStream` for streaming
- No SSE-C, no glacier restore, no multipart download, no request payer

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 3.1 | **Glacier object restore not supported** | LOW | Go detects GLACIER storage class errors and triggers restore with polling. Rust does not handle this. Only relevant for users archiving to Glacier class. |
| 3.2 | **SSE-C on GetObject not supported** | MEDIUM | If SSE-C is used for upload, it must also be provided on download. Missing this would make SSE-C completely non-functional. Linked to gap 2.1. |

---

## 4. Delete Operations

### Go Implementation (`S3.deleteKeys`, lines 421-509; `S3.deleteKey` lines 352-379)
- **Versioned bucket support**: For versioned buckets, lists all object versions and deletes each version individually
- **Batch deletion**: Uses `DeleteObjects` API with 1000-key batches
- **Delete concurrency**: `DeleteConcurrency` config for parallel version listing
- **Quiet mode**: `Quiet: true` on batch deletes
- **Per-object error handling**: Parses `BatchDeleteError` with individual key failures
- **RequestPayer** and **ChecksumAlgorithm** on deletes
- **RequestContentMD5**: Optional Content-MD5 header for S3-compatible storage (custom middleware to replace flexible checksums)
- **Object disk path**: Separate `DeleteFileFromObjectDiskBackup` using `ObjectDiskPath` prefix

### Rust Implementation (`S3Client::delete_objects`, lines 481-516)
- Batch deletion with 1000-key chunks
- No versioning support
- No per-object error handling (entire batch fails or succeeds)
- No quiet mode
- No request payer, checksum algorithm, content MD5

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 4.1 | **Versioned bucket delete not supported** | MEDIUM | Go lists all object versions and deletes each. Rust only deletes current version. Versioned buckets will accumulate delete markers and old versions. |
| 4.2 | **Per-object delete error handling missing** | LOW | Go parses individual key failures from batch delete response. Rust treats entire batch as success/fail. |
| 4.3 | **RequestContentMD5 not supported** | LOW | Some S3-compatible storage (e.g., certain MinIO configs) require Content-MD5 on delete. Go has a custom middleware for this. |
| 4.4 | **DeleteConcurrency not configurable** | LOW | Go has separate `delete_concurrency` for parallel version listing. Rust does not need this without versioning support, but the config field is missing. |

---

## 5. Head/Stat Operations

### Go Implementation (`S3.StatFile*`, lines 603-627)
- Returns `RemoteFile` with size, last_modified, storage_class, name
- SSE-C parameters on HeadObject
- RequestPayer on HeadObject
- Proper HTTP 404 detection via smithy error types

### Rust Implementation (`S3Client::head_object`, lines 524-555)
- Returns `Option<u64>` (just size)
- 404 detection via `is_not_found()`
- No SSE-C, no request payer

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 5.1 | **Head returns only size, not last_modified or storage_class** | LOW | Go returns a full `RemoteFile` struct. Rust only returns size. Storage class is used for Glacier detection. |

---

## 6. List/Walk Operations

### Go Implementation (`S3.Walk*`, `S3.remotePager`, lines 629-691)
- Uses `ListObjectsV2Paginator` with MaxKeys=1000
- Supports recursive and non-recursive (with delimiter) listing
- Channel-based concurrent processing
- Returns `RemoteFile` with size, last_modified, storage_class, name

### Rust Implementation (`S3Client::list_objects`, `list_common_prefixes`, lines 370-455)
- Manual pagination with continuation_token
- Separate methods for recursive (list_objects) and prefix-only (list_common_prefixes)
- Returns `S3Object` with key, size, last_modified

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 6.1 | **Storage class not returned in list results** | LOW | Go `s3File` includes `storageClass`. Rust `S3Object` does not. Minor, but used for Glacier detection in Go. |

---

## 7. CopyObject Operations

### Go Implementation (`S3.CopyObject`, lines 693-821)
- Destination key uses `ObjectDiskPath` prefix (not main `Path`)
- **GCS bypass**: If endpoint contains `storage.googleapis.com`, skips multipart copy regardless of size
- **Multipart copy threshold**: 5 GiB
- **Multipart part size**: Calculated like upload, with `AdjustValueByRange(partSize, 128*1024*1024, 5*1024*1024*1024)` -- 128 MiB minimum for copy parts
- **Concurrent copy parts**: Uses `errgroup` with `s.Config.Concurrency` limit
- **Checksum preservation**: Copies checksum fields (CRC32, CRC32C, SHA1, SHA256) from UploadPartCopy response
- `enrichCopyObjectParams`: SSE (all 6 fields), RequestPayer, ChecksumAlgorithm, ObjectLabels/Tagging
- `enrichCreateMultipartUploadParams`: SSE, RequestPayer, ChecksumAlgorithm, ObjectLabels/Tagging
- Sort parts by number before complete

### Rust Implementation (`S3Client::copy_object*`, lines 769-1060)
- Size-check via head_object, multipart if >5 GiB
- **Sequential copy parts** (no parallelism within multipart copy)
- Minimum chunk size: 5 MiB (not 128 MiB)
- SSE (AES256/aws:kms), ACL, storage class on copy
- Streaming fallback on copy failure
- Retry with exponential backoff and jitter

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 7.1 | **Multipart copy parts not parallelized** | MEDIUM | Go uses `errgroup` with `Concurrency` limit for parallel part copies. Rust copies parts sequentially. For multi-GB objects this is significantly slower. |
| 7.2 | **Multipart copy minimum chunk size too small** | LOW | Go uses 128 MiB minimum for copy parts. Rust uses 5 MiB. Smaller chunks mean more API calls and more overhead. |
| 7.3 | **GCS multipart copy bypass missing** | LOW | Go skips multipart copy for GCS endpoints. Rust does not detect GCS. Not in scope for S3-only but worth noting. |
| 7.4 | **Checksum fields not preserved on copy parts** | LOW | Go copies CRC32/CRC32C/SHA1/SHA256 from UploadPartCopy response. Rust only copies ETag. |
| 7.5 | **SSE-C not applied to CopyObject** | MEDIUM | Go applies all 6 SSE fields to copy. Linked to gap 2.1. |
| 7.6 | **Object labels/tagging not applied to CopyObject** | MEDIUM | Go applies `ObjectLabels` as S3 tagging on copy. Linked to gap 2.3. |

---

## 8. SSE Encryption (Summary)

### Go: 6 SSE config fields
1. `SSE` -- server-side encryption algorithm (e.g., `aws:kms`, `AES256`)
2. `SSEKMSKeyId` -- KMS key ID for aws:kms
3. `SSECustomerAlgorithm` -- algorithm for customer-provided keys
4. `SSECustomerKey` -- customer-provided encryption key
5. `SSECustomerKeyMD5` -- MD5 of customer-provided key
6. `SSEKMSEncryptionContext` -- additional KMS encryption context

Applied on: PutObject, CreateMultipartUpload, CopyObject, GetObject, HeadObject

### Rust: 2 SSE config fields
1. `sse` -- "AES256" or "aws:kms"
2. `sse_kms_key_id` -- KMS key ID

Applied on: PutObject, CreateMultipartUpload, CopyObject (not GetObject/HeadObject since server-managed SSE does not require it on read)

### Gap Summary
- **SSE-C (Customer-provided keys)**: 3 missing fields. Affects Put, Get, Head, Copy.
- **SSE KMS Encryption Context**: 1 missing field. Minor.

---

## 9. Storage Class

### Go Implementation
- `StorageClass`: uppercased string
- `UseCustomStorageClass` + `CustomStorageClassMap`: Allows per-file storage class overrides based on file path patterns

### Rust Implementation
- `storage_class`: uppercased string

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 9.1 | **Custom storage class map not supported** | LOW | Go supports `UseCustomStorageClass` with a `CustomStorageClassMap` for per-pattern storage class. Rust uses a single global class. Niche feature. |

---

## 10. Rate Limiting

### Go Implementation
- `BackupDestination.throttleSpeed()` in `general.go` (lines 583-596)
- Post-hoc throttle: After each file transfer, calculates actual speed, sleeps if above `maxSpeed`
- Applied per-file, not per-byte

### Rust Implementation
- Token-bucket `RateLimiter` (in `src/rate_limiter.rs`)
- Per-byte rate limiting with refill intervals
- Shared via `Arc` across concurrent tasks

### Assessment
**Rust is BETTER here.** Token-bucket is more precise and consistent than Go's post-hoc sleep approach. Go's method allows burst traffic per-file with correction only after file completes. No gap.

---

## 11. Retry Logic

### Go Implementation
- SDK-level: `RetryModeStandard` on AWS config
- Application-level: Uses `go-resiliency/retrier` with `ExponentialBackoff` + `AddRandomJitter`
- Applied externally in `general.go` operations (UploadPath, DownloadPath, RemoveBackupRemote)

### Rust Implementation
- Custom retry in `copy_object_with_retry_jitter()`: 3 attempts, [100ms, 400ms, 1600ms] backoff with jitter
- Application-level retry for CRC64 verification failures during download

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 11.1 | **No SDK-level retry mode configured** | LOW | Go sets `RetryModeStandard`. Rust relies on SDK defaults. May already default to standard retry. |

---

## 12. STS AssumeRole

### Go Implementation
- Uses `stscreds.NewAssumeRoleProvider` which wraps STS and **auto-refreshes** credentials before expiry
- Chains with IRSA: IRSA -> AssumeRole chaining for cross-account from EKS
- Priority handling for multiple ARN sources

### Rust Implementation
- One-shot `sts_client.assume_role().send()` extracting temporary credentials
- Stored as static credentials with no refresh

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 12.1 | **STS credential auto-refresh missing** | HIGH | One-shot credentials expire (default 1h). Long-running server/watch mode will fail. Go auto-refreshes. This is the most critical gap for production use. |
| 12.2 | **IRSA -> AssumeRole chaining missing** | MEDIUM | EKS workloads that need cross-account access use IRSA to get a base identity, then AssumeRole to the target account. Rust does not support this chain. |

---

## 13. Object Disk Path Handling

### Go Implementation
- `ObjectDiskPath` is a separate config field (distinct from `Path`)
- Used as key prefix for CopyObject destinations: `dstKey = path.Join(s.Config.ObjectDiskPath, dstKey)`
- `DeleteFileFromObjectDiskBackup` uses `ObjectDiskPath` prefix
- `DeleteKeysFromObjectDiskBackupBatch` uses `ObjectDiskPath` prefix
- In object_disk.go: Complex URL parsing to extract bucket, region, and path from ClickHouse disk endpoint URLs (handles virtual-hosted, path-style, FIPS, and GCS endpoints)

### Rust Implementation
- `object_disk_path` stored as field with public getter
- Used by callers (not internally by S3Client methods)
- Object disk metadata parsing handles all 5 format versions

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 13.1 | **No automatic ObjectDiskPath prefix on CopyObject** | LOW | Go's `CopyObject` internally prepends `ObjectDiskPath`. Rust relies on callers to handle this. This is an architectural difference, not necessarily a bug, but callers must be aware. |
| 13.2 | **No separate delete method for object disk path** | LOW | Go has `DeleteFileFromObjectDiskBackup` and `DeleteKeysFromObjectDiskBackupBatch` that use `ObjectDiskPath`. Rust does not differentiate. |
| 13.3 | **No disk endpoint URL parsing** | MEDIUM | Go's `object_disk.go` has sophisticated URL parsing (`makeObjectDiskConnection`) to extract bucket/region/path from ClickHouse disk endpoint configurations. Handles virtual-hosted, path-style, S3 Express, FIPS, and GCS URLs. Rust does not parse disk endpoint URLs at all -- it relies on the backup config, not the disk config. |
| 13.4 | **No per-disk S3 connection** | MEDIUM | Go creates per-disk S3 connections with per-disk credentials (from ClickHouse XML config). Supports different buckets/regions per disk. Rust uses a single S3 client for all operations. |

---

## 14. Custom Endpoint Support

### Go Implementation
- Custom `ResolveEndpoint()` implementing `EndpointResolverV2`
- `DisableSSL` for HTTP endpoints
- `DisableHTTPS` on endpoint options
- GCS compatibility layer

### Rust Implementation
- `endpoint_url()` on both SDK config loader and S3 config builder
- `force_path_style` on S3 config
- No `DisableSSL` wiring (field exists in config but unused)

### Gaps

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| 14.1 | **DisableSSL not wired** | LOW | Config field exists but is not used. HTTP endpoints via endpoint URL may work, but explicit disable is not configured. |

---

## 15. Additional Go Features Not in Rust

| Feature | Go Location | Severity | Notes |
|---------|------------|----------|-------|
| **Metadata cache** | `general.go:136-217` | LOW | Go caches backup metadata in a temp file for faster listing. Rust re-fetches each time. |
| **Compression format variety** | `utils.go` | LOW | Go supports tar, lz4, bzip2, gzip, snappy, xz, brotli, zstd. Rust supports lz4 only. Design decision per spec. |
| **Custom storage class map** | S3Config | LOW | Per-pattern storage class overrides. Niche feature. |
| **AllowMultipartDownload** | S3Config | LOW | Parallel range-based download. Not in Rust. |
| **RequestContentMD5** | S3Config | LOW | For S3-compatible storage requiring Content-MD5 headers. |
| **DeleteBatchSize** | general.go | LOW | Configurable batch size for remote backup removal (default from config). Rust uses fixed 1000. |
| **Object disk per-disk connections** | object_disk.go | MEDIUM | Go creates separate S3/Azure connections per ClickHouse disk with per-disk credentials from XML config. Rust uses a single client. |
| **Macro expansion in paths** | general.go:598-631 | LOW | Go applies ClickHouse macros to `Path` and `ObjectDiskPath`. Rust handles macro expansion at a different level. |

---

## Priority Summary

### HIGH Priority (production correctness)
1. **12.1 - STS credential auto-refresh**: One-shot assume-role credentials expire after 1h. Server/watch mode will break.

### MEDIUM Priority (feature gaps affecting real users)
2. **1.1 - IRSA support**: Critical for EKS deployments.
3. **1.2 - AssumeRole auto-refresh**: Same root cause as 12.1.
4. **2.1 + 3.2 + 7.5 - SSE-C (customer-provided keys)**: 3 config fields missing across all operations. If any user needs SSE-C, nothing works.
5. **2.3 + 7.6 - Object tagging/labels**: Used for cost allocation and lifecycle policies.
6. **4.1 - Versioned bucket delete**: Leaves garbage in versioned buckets.
7. **7.1 - Parallel multipart copy parts**: Performance gap for large objects.
8. **13.3 + 13.4 - Per-disk S3 connections**: Multi-disk setups with different buckets/regions/creds won't work.

### LOW Priority (nice-to-have, edge cases)
9. All items marked LOW in the tables above.

---

## Architectural Differences (Not Gaps)

These are intentional design differences, not gaps:

1. **Upload strategy**: Go uses `s3manager.Uploader` (high-level, streaming). Rust buffers in memory then uses PutObject or manual multipart. Both work; Go is more memory-efficient for large objects but Rust's approach is simpler.
2. **Rate limiting**: Rust's token-bucket is better than Go's post-hoc throttle.
3. **Key prefix naming**: Go uses `Path`, Rust uses `prefix`. Same concept.
4. **Object disk metadata**: Both parse all 5 format versions correctly. Rust has unit tests; Go has integration coverage.
5. **Single S3 client vs per-disk**: Rust's single-client design is simpler and works when all disks share the same S3 bucket/region. Go's per-disk approach handles heterogeneous disk configurations.
