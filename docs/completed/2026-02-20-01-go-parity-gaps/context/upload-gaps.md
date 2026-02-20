# Upload Flow: Go vs Rust Parity Analysis

Comparison of `pkg/backup/upload.go` + `pkg/storage/general.go` + `pkg/storage/s3.go`
against `src/upload/mod.rs` + `src/upload/stream.rs` + `src/storage/s3.rs`.

---

## 1. Compression Methods

### Go
- Supports **8 compression formats**: tar (uncompressed), lz4, bzip2, gzip, sz (snappy), xz, brotli, zstd.
- Configured per-storage-backend (e.g., `s3.compression_format`, `gcs.compression_format`).
- Default: `"tar"` (uncompressed).
- `compression_level` is per-backend (default: 1).
- `GetArchiveExtension()` maps format to extension string (e.g., `"tar.lz4"`, `"tar.bz2"`, `"tar.sz"`, `"tar.xz"`, `"tar.br"`).

### Rust
- Supports **4 compression formats**: lz4, zstd, gzip, none (raw tar).
- Configured via `config.backup.compression` (single field, not per-backend since S3-only).
- Default: `"lz4"` (per design doc, not `"tar"` like Go).
- `compression_level` from `config.backup.compression_level`.
- `archive_extension()` maps format to extension.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| Missing `bzip2` format | Low | Rarely used. Would require `bzip2` crate. |
| Missing `sz` (snappy) format | Low | Rarely used. Would require `snap` crate. |
| Missing `xz` format | Low | Rarely used. Would require `xz2` crate. |
| Missing `brotli` format | Low | Rarely used. Would require `brotli` crate. |
| Default compression differs: Go=`"tar"`, Rust=`"lz4"` | Info | Intentional design decision. LZ4 is better default for new installs. Go default of raw tar was kept for backward compatibility. Not a gap to fix. |

**Verdict**: The 4 missing formats (bzip2, snappy, xz, brotli) are very rarely used in production ClickHouse backup workflows. LZ4 and zstd cover >99% of real-world usage. Low priority.

---

## 2. Upload Modes: upload_by_part vs upload_by_size

### Go
Two distinct upload modes controlled by `general.upload_by_part`:

- **`upload_by_part = true`** (default since v2.4+): Each part directory is archived/compressed individually as its own S3 object. S3 key includes the part name. This is required for `--diff-from-remote` to work (part-level identity).
- **`upload_by_part = false`**: Files from multiple parts are batched by cumulative size (`general.max_file_size`, default 1GB). Creates numbered archives (e.g., `default_1.tar.lz4`, `default_2.tar.lz4`) containing files from potentially many parts. More efficient for lots of small parts.

The splitting logic is in `splitPartFiles()` which calls either `splitFilesByName()` (per-part) or `splitFilesBySize()` (batched).

### Rust
- **Always uploads per-part**: Each part is individually compressed and uploaded as a single S3 object. This is equivalent to Go's `upload_by_part = true`.
- No concept of batching multiple parts into a single archive.
- No `upload_by_part` or `max_file_size` config fields.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| No `upload_by_part = false` (size-batched) mode | Low | Go's per-part mode is the recommended default and required for incremental backups. Size-batching is a legacy optimization for many tiny parts. Since Rust always uses per-part mode, incremental backups work. |
| No `max_file_size` config field | Low | Only meaningful for size-batched mode. |

**Verdict**: Not a real parity gap. Go's recommended mode (`upload_by_part = true`) matches Rust's behavior exactly. The size-batched fallback is legacy.

---

## 3. Compression Format `"none"` / Directory Upload

### Go
When `compression_format = "none"`:
- `upload_by_part` must be `true` (validated at upload time).
- Files are uploaded individually via `UploadPath()` without any tar wrapping.
- Each file within a part is a separate S3 object.
- S3 key structure: `{backup_name}/shadow/{db}/{table}/{disk}/{part_name}/{file_path}`.

### Rust
When `compression = "none"`:
- Still wraps in a tar archive (`.tar` extension, no compression).
- Uploads a single `.tar` file per part (consistent with other formats).
- S3 key: `{backup_name}/data/{db}/{table}/{part_name}.tar`.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| Rust tar-wraps `"none"` format; Go uploads raw files | Medium | Behavioral difference: Go's `"none"` produces individual files on S3, Rust produces `.tar` bundles. This means a backup created by Go with `none` compression cannot be downloaded by Rust and vice versa. However, this is a niche scenario since `none` compression is very rarely used in production. |
| Different S3 key prefix: Go uses `shadow/`, Rust uses `data/` | Medium | These two implementations are not cross-compatible anyway due to different manifest formats. This is expected. |

**Verdict**: The `"none"` format difference is real but low-impact since (a) `none` compression is rarely used, and (b) cross-tool compatibility is not a goal.

---

## 4. Multipart Upload Logic

### Go
- Uses `s3manager.NewUploader()` from AWS SDK v2, which handles multipart automatically.
- `uploader.Concurrency` = `s3.concurrency` (within-file parallelism for multipart chunks).
- `uploader.BufferProvider` uses a pooled buffer.
- `uploader.PartSize` calculated from `ChunkSize` and `MaxPartsCount` with `AdjustValueByRange(partSize, 5MB, 5GB)`.
- The Go SDK manager automatically decides single vs multipart based on stream length.

### Rust
- Manual multipart threshold: parts > 32 MiB use multipart.
- `calculate_chunk_size()` computes chunk size from `config.s3.chunk_size` and `config.s3.max_parts_count`, clamped to minimum 5 MiB.
- Multipart flow: `create_multipart_upload` -> sequential `upload_part` per chunk -> `complete_multipart_upload`.
- On error: `abort_multipart_upload` for cleanup.
- No within-file parallelism (chunks uploaded sequentially within a single part).

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| No within-file multipart parallelism | Medium | Go uses `s3manager.Concurrency` to upload multiple chunks of the same file in parallel. Rust uploads chunks sequentially. For very large parts (>1 GB), this is slower. The `s3.concurrency` config field exists in Rust's S3Client but is stored without being used during multipart upload. |
| No upper bound on chunk size (5 GB) | Low | Go clamps to `[5MB, 5GB]` via `AdjustValueByRange`. Rust only enforces the 5 MB minimum. In practice, auto-calculated chunks rarely exceed 5 GB. |
| Hardcoded 32 MiB multipart threshold | Low | Go relies on the SDK manager's built-in threshold. The 32 MiB value is reasonable and matches the SDK default. Not a real gap. |
| No buffered read-seeker pool | Low | Go uses `s3manager.NewBufferedReadSeekerWriteToPool` for memory efficiency during multipart. Rust buffers the entire compressed part in memory before deciding single vs multipart. For extremely large parts this uses more memory, but the current approach is simpler and works fine for typical part sizes. |

**Verdict**: The main gap is within-file multipart parallelism. For parts exceeding ~500 MB compressed, Go's parallel chunk upload would be notably faster. The `concurrency` field already exists in `S3Client` but is unused during `upload_part`. This could be addressed by spawning multiple `upload_part` calls concurrently within a `JoinSet`.

---

## 5. S3 Disk Part Handling (CopyObject)

### Go
- `CopyObject()` in `pkg/storage/s3.go`:
  - For objects under 5 GB or GCS endpoints: single `CopyObject` API call.
  - For objects >= 5 GB: multipart copy using `UploadPartCopy` with range-based splitting.
  - Applies `ObjectDiskPath` prefix to destination key.
  - ACL, SSE, storage class applied to destination.
  - Error handling with abort on multipart failure.

### Rust
- `copy_object()` in `src/storage/s3.rs`:
  - Checks source size via `head_object()`; objects > 5 GiB use `copy_object_multipart()`.
  - Multipart copy with `upload_part_copy`, byte-range splitting, auto chunk size calculation.
  - Abort on error.
  - `copy_object_with_retry_jitter()`: 3 retries with exponential backoff + configurable jitter.
  - Conditional streaming fallback (download + re-upload) when `allow_object_disk_streaming` is true.
  - ACL, SSE, storage class applied.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| `object_disk_path` not used in CopyObject dest key | Medium | Go's `CopyObject` prepends `s3.object_disk_path` to the destination key (`dstKey = path.Join(s.Config.ObjectDiskPath, dstKey)`). Rust stores `object_disk_path` in `S3Client` but does not use it in `copy_object()`. The upload code builds dest keys as `{backup_name}/objects/{path}` without considering `object_disk_path`. This could cause incorrect key paths when `object_disk_path` is configured. |
| No GCS-specific single-copy path | Low | Go checks for GCS endpoints and forces single CopyObject even for large files (GCS has different multipart semantics). Rust always uses the S3 multipart path. Not relevant since Rust is S3-only. |

**Verdict**: The `object_disk_path` prefix gap is real and could affect users with non-default S3 disk configurations. Should be investigated.

---

## 6. Resume Logic

### Go
- `resumableState` in `pkg/resumable/State`:
  - File-based state tracking with `AppendToState(key, size)` and `IsAlreadyProcessed(key) -> (bool, int64)`.
  - `adjustResumeFlag()`: auto-enables resume when `config.general.use_resumable_state` is true.
  - Resume state tracks both the S3 key and the uploaded size.
  - `Close()` called in defer to flush state.
  - Parameters hash validated on resume (same as Rust).

### Rust
- `resume.rs` with `UploadState`:
  - `completed_keys: HashSet<String>` (only tracks key, not size).
  - `params_hash` for invalidation.
  - `backup_name` for validation.
  - Atomic write via `.tmp` + rename.
  - `save_state_graceful()` is non-fatal on write failure.
  - State loaded at start, keys checked during work queue construction.
  - State saved after each successful part upload (within mutex lock).
  - State deleted on successful completion.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| Rust does not track uploaded byte size in resume state | Low | Go tracks `(key, size)` so it can contribute to `compressedDataSize` accounting on resume. Rust only tracks key existence. This means resumed uploads may report inaccurate `compressed_size` in the manifest (missing the sizes of previously uploaded parts). |
| Resume auto-enable differs | Low | Go's `adjustResumeFlag` auto-enables resume when `use_resumable_state=true` even without `--resume` CLI flag. Rust requires both `--resume` AND `use_resumable_state`. This matches the design doc more strictly (resume is opt-in). |
| No `Close()` call for flush | Low | Go calls `resumableState.Close()` in defer. Rust saves state after each part atomically. Not a real gap -- Rust's approach is actually more crash-safe. |

**Verdict**: Minor differences. The byte-size tracking gap could cause inaccurate manifest metadata on resume, but does not affect correctness. Low priority.

---

## 7. Rate Limiting

### Go
- `throttleSpeed()` function: Calculates sleep duration based on `(actual_speed - max_speed)`.
- Applied per-file in `UploadPath()` and per-stream in `UploadCompressedStream()`.
- `upload_max_bytes_per_second` config field.
- Rate limiting is per-transfer (not shared across concurrent uploads).

### Rust
- `RateLimiter` (token bucket): Shared across all concurrent upload tasks via `Arc<Mutex>`.
- `consume(bytes)` method with burst allowance (1 second of tokens).
- Applied after each part upload in the parallel pipeline.
- `backup.upload_max_bytes_per_second` config field.
- 0 = unlimited (no-op passthrough).

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| Different rate limiting scope | Info | Go rate-limits per individual transfer. Rust rate-limits globally across all concurrent uploads. Rust's approach is arguably better (total bandwidth control) but produces different behavior. Not a gap to fix -- Rust's approach is superior. |
| Rate limiter applied post-upload in Rust | Low | Rust calls `rate_limiter.consume(compressed_size)` AFTER the upload completes. This means the first N parts upload at full speed until the bucket drains. Go applies throttling during the stream. For small parts this makes no practical difference; for very large parts there could be a burst at the start. |

**Verdict**: Different but acceptable. Rust's global token bucket is actually better for total bandwidth control. The post-upload application is a minor behavioral difference.

---

## 8. Manifest Upload Atomicity

### Go
- Direct `PutFile()` to `{backup_name}/metadata.json`.
- With retry: `retrier.ExponentialBackoff(RetriesOnFailure, AddRandomJitter(...))`.
- **No atomic pattern** (no `.tmp` + CopyObject + delete).

### Rust
- Three-step atomic pattern:
  1. Upload to `{backup_name}/metadata.json.tmp`
  2. `CopyObject` from `.tmp` to final key
  3. Delete `.tmp`
- Crash between steps 1 and 2: backup is "broken" (no manifest), cleaned by `clean_broken`.
- Crash between steps 2 and 3: `.tmp` is orphaned but harmless.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| Go has no atomic manifest upload | Info | This is a Rust improvement over Go, not a gap. Go's direct PutFile means a crash during manifest upload could leave a partially written manifest. Rust's approach is more robust. |
| Rust has no retry on manifest upload | Medium | Go retries manifest PutFile with exponential backoff + jitter. Rust does a single attempt for each of the three atomic steps. If the `.tmp` upload or the CopyObject fails transiently, the entire upload fails. |

**Verdict**: Rust is better on atomicity but worse on retry. Adding retry to the manifest upload steps would close the gap.

---

## 9. Diff-From (Incremental) Logic

### Go
- `--diff-from` (local base): `getTablesDiffFromLocal()` reads base backup metadata, `markDuplicatedParts()` marks parts as `Required=true` (skipped in upload).
- `--diff-from-remote` (remote base): `getTablesDiffFromRemote()` fetches remote backup metadata from storage, same marking.
- Diff comparison: by part name only (no CRC64 check in Go's `markDuplicatedParts`).
- Optional local file-level verification: when `--diff-from` (local), `IsDuplicatedParts()` compares actual files on disk.
- `RequiredBackup` field in manifest tracks the base backup name.

### Rust
- `--diff-from` (local base): Applied during `backup::create()` by calling `diff_parts()`.
- `--diff-from-remote` (remote base): Applied during `upload()` by fetching remote manifest and calling `diff_parts()`.
- Diff comparison: by `(table_key, disk_name, part_name, checksum_crc64)` -- more precise than Go.
- CRC64 mismatch detection: warns and re-uploads when same name but different checksum.
- No `RequiredBackup` / `required_backups` field in manifest (incremental chain tracking uses different mechanism).

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| Go diff uses part name only; Rust uses name + CRC64 | Info | Rust is more correct. Not a gap to fix. |
| No `RequiredBackup` field in Rust manifest | Medium | Go sets `backupMetadata.RequiredBackup = diffFrom` or `diffFromRemote`. This field is used by remote retention to protect base backups from deletion (incremental chain protection). Rust's incremental chain protection uses a different mechanism in `retention_remote()` but the `required_backups` field behavior should be verified. |
| Go has `IsDuplicatedParts()` file-level check for local diff | Low | Go compares actual file contents when using `--diff-from` (local). Rust trusts CRC64 checksums. CRC64 is sufficient for practical purposes. |
| No `--diff-from` for local base during upload | Info | Rust applies `--diff-from` during `create`, not `upload`. Go applies during `upload`. The end result is the same (fewer parts uploaded). Different code path, same semantics. |

**Verdict**: Rust's diff logic is actually stronger (CRC64 comparison vs name-only). The `RequiredBackup` manifest field absence should be verified against retention behavior.

---

## 10. Error Handling and Cleanup

### Go
- Per-table upload errors propagated via `errgroup.Wait()`.
- Retry at the per-file level via `retrier.ExponentialBackoff`.
- No explicit cleanup on upload failure (partially uploaded data remains on S3).
- `RemoveOldBackupsRemote()` and `RemoveOldBackupsLocal()` called after successful upload.
- `deleteSource` flag: deletes local files after each part upload (per-file granularity, within the upload goroutine).

### Rust
- All upload tasks collected via `try_join_all` (fail-fast on first error).
- Retry at CopyObject level (3 retries with backoff + jitter).
- Multipart upload: `abort_multipart_upload` on error.
- No retry on PutObject or manifest upload.
- `delete_local` flag: deletes entire local backup directory after ALL uploads complete.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| No PutObject retry | Medium | Go retries every file upload with exponential backoff. Rust has no retry on `put_object` (single part upload) or multipart `upload_part`. Only CopyObject has retry. A transient S3 error on a single PutObject will fail the entire upload. |
| No per-part delete-source | Low | Go can delete local files immediately after each part uploads (saves disk space during upload). Rust deletes the entire backup directory only after all parts succeed. Rust's approach is safer but uses more disk space during upload. |
| No partial upload cleanup on failure | Low | Neither Go nor Rust cleans up partially uploaded parts on failure. The broken backup is left on S3. Go has `clean_broken` for cleanup; Rust also has `clean_broken`. Parity. |
| No automatic retention after upload | Info | Go calls `RemoveOldBackupsRemote` and `RemoveOldBackupsLocal` inline after successful upload. Rust handles retention as separate commands or in the watch loop. Design choice, not a gap. |

**Verdict**: The main gap is PutObject retry. Adding retry with exponential backoff to `put_object` and `upload_part` calls would match Go behavior and improve reliability.

---

## 11. RBAC / Config / Named Collections Upload

### Go
- `uploadRBACData()`: Compresses RBAC files to `access.tar.{ext}` or uploads raw files (if `none`).
- `uploadConfigData()`: Same pattern for config files.
- `uploadNamedCollections()`: Same pattern.
- Resume-aware: checks `IsAlreadyProcessed()` before uploading.
- Size tracked in manifest (`backupMetadata.RBACSize`, `ConfigSize`).

### Rust
- `upload_simple_directory()`: Uploads raw files (no compression) for both `access/` and `configs/` directories.
- Sequential upload (small files, no parallelism needed).
- Not resume-aware (these directories are uploaded after all data parts).
- Sizes not tracked separately in manifest (`rbac_size` and `config_size` are hardcoded to 0 in list API).

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| RBAC/config files not compressed | Low | Go compresses these into a single archive. Rust uploads raw files. For small RBAC/config files this makes no practical difference. |
| No resume tracking for RBAC/config upload | Low | These are small files that upload quickly. Not a real issue. |
| `rbac_size` and `config_size` not tracked in manifest | Low | Already noted as a known limitation in CLAUDE.md. |

**Verdict**: Minor gaps, low priority.

---

## 12. Table Pattern and Partition Filtering During Upload

### Go
- `prepareTableListToUpload()` applies table pattern filter to select which tables to upload.
- `partitions` parameter filters which parts within a table to upload.
- `skipProjections` parameter filters projection directories during `splitPartFiles()`.

### Rust
- Upload does not filter by table pattern or partitions -- it uploads ALL parts in the manifest.
- Table pattern filtering happens at `create` time, not upload time.
- Projection filtering happens at `create` time.
- Upload simply processes whatever the manifest contains.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| No table pattern filter during upload | Low | Rust's approach is cleaner: `create` produces a manifest with exactly the tables/parts to back up, `upload` processes the manifest. Go's approach allows creating a full backup but uploading a subset. The Rust approach means you must create the backup with the right filter from the start. |
| No partition filter during upload | Low | Same as above. Partition filtering at create time is sufficient. |

**Verdict**: Design difference, not a gap. Rust's approach (filter at create, upload all) is simpler and equally correct.

---

## 13. Embedded Backup Support

### Go
- Full support for `embedded_backup_disk` (BACKUP TO DISK): special handling for embedded backup metadata paths, `.backup` file upload, different metadata paths.
- `isEmbedded` flag controls many code paths in upload.

### Rust
- No embedded backup support (not in design scope -- Rust is a standalone FREEZE-based backup tool).

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| No embedded backup support | Info | Intentionally out of scope. Rust uses FREEZE-based backup exclusively. Embedded backups (`BACKUP TO DISK`) are a different ClickHouse feature. Not a gap to fix. |

---

## 14. Custom Storage / Non-S3 Backends

### Go
- `RemoteStorage` interface supports S3, GCS, Azure, COS, FTP, SFTP, custom.
- Upload code checks for `remote_storage == "custom"` and delegates to custom handler.

### Rust
- S3-only by design.

### Gaps

| Gap | Severity | Notes |
|-----|----------|-------|
| No non-S3 storage backends | Info | Intentional design decision. Not a gap. |

---

## Summary: Actionable Gaps by Priority

### High Priority
(none)

### Medium Priority
1. **No PutObject/UploadPart retry** -- Go retries every upload with exponential backoff. A transient S3 error on a single part fails the entire Rust upload. Add retry wrapper around `put_object` and `upload_part`.
2. **No retry on manifest upload steps** -- The atomic three-step manifest upload has no retry on any step. Add retry with backoff.
3. **`object_disk_path` not applied in CopyObject destination** -- Go prepends `s3.object_disk_path` to CopyObject dest keys. Rust stores the value but does not use it.
4. **No within-file multipart parallelism** -- Go uses `s3manager.Concurrency` for parallel chunk uploads. Rust uploads chunks sequentially. The `concurrency` field exists but is unused.

### Low Priority
5. Missing compression formats: bzip2, snappy, xz, brotli.
6. No upload_by_part=false (size-batched) mode.
7. Resume state does not track uploaded byte sizes (affects reported `compressed_size` on resume).
8. `"none"` compression format produces tar in Rust vs raw files in Go.
9. Rate limiter applied post-upload (burst behavior differs).
10. RBAC/config files not compressed during upload.
11. Per-part delete-source (Go deletes files within upload goroutine; Rust waits until end).

### Not Gaps (Design Differences)
- Default compression format (lz4 vs tar).
- Table/partition filter during upload (Rust filters at create time).
- Embedded backup support (out of scope).
- Non-S3 storage backends (out of scope).
- Manifest atomicity (Rust is better than Go).
- Diff comparison using CRC64 (Rust is more precise than Go).
- Global vs per-transfer rate limiting (Rust's approach is better).
