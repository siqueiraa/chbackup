# Data Authority Analysis

Phase 2a is primarily about parallelism (concurrency control, semaphores) and multipart upload. It does not introduce new data tracking or calculations beyond what Phase 1 already provides. The analysis below confirms no over-engineering risk.

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Upload concurrency limit | Config | `backup.upload_concurrency` (u32) | USE EXISTING |
| Download concurrency limit | Config | `backup.download_concurrency` (u32) | USE EXISTING |
| FREEZE/restore concurrency limit | Config | `clickhouse.max_connections` (u32) | USE EXISTING |
| Rate limit (bytes/sec) | Config | `backup.upload_max_bytes_per_second` (u64) | USE EXISTING |
| Rate limit download | Config | `backup.download_max_bytes_per_second` (u64) | USE EXISTING |
| Multipart threshold | Design doc | 32MB uncompressed default | MUST IMPLEMENT -- no config field exists for multipart_threshold; use constant or derive from `s3.chunk_size` |
| Multipart chunk size | Config | `s3.chunk_size` (u64, 0=auto) | USE EXISTING |
| Max parts count | Config | `s3.max_parts_count` (u32, default 10000) | USE EXISTING |
| Part uncompressed size | PartInfo | `size` (u64) | USE EXISTING |
| Engine type for ATTACH strategy | TableManifest | `engine` (String) | USE EXISTING |
| Sequential vs parallel ATTACH | Function | `needs_sequential_attach(engine)` | USE EXISTING |

## Analysis Notes

- The multipart upload threshold (32MB) is specified in the design doc section 3.6 but has no dedicated config field. The `s3.chunk_size` (default 0 = auto) controls multipart _chunk_ size, not the decision threshold. A constant `MULTIPART_THRESHOLD: u64 = 32 * 1024 * 1024` is appropriate.
- Rate limiting requires a new token bucket implementation (nothing exists). This is a MUST IMPLEMENT item.
- All concurrency parameters already exist in Config and are validated (> 0).
- The `needs_sequential_attach()` function already classifies engines correctly for the parallel vs sequential ATTACH decision.
