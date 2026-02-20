# Download Flow: Go vs Rust Comparison

## Summary

The Rust download implementation covers the core download pipeline well -- manifest
fetch, parallel part download with semaphore, LZ4/zstd/gzip decompression, CRC64
verification with retry, S3 disk metadata-only download, resume state, hardlink
dedup, disk space pre-flight, and progress bar. However, there are several
behavioral gaps compared to the Go implementation.

---

## 1. Download Logic and Parallel Strategy

### Go Behavior
- Two-level parallelism: first downloads **table metadata** in parallel (bounded
  by `DownloadConcurrency`), then downloads **table data** in parallel with the
  same bound.
- The table metadata download phase runs as a separate `errgroup` before the
  data download phase.
- Each table's data download runs as one goroutine in the data group; within
  `downloadTableData`, archive files or directory parts are dispatched into
  yet another `errgroup` with the same concurrency limit.
- Supports two data formats: **archive mode** (tar+compression per file in
  `table.Files`) and **directory mode** (`DirectoryFormat`, per-part directory
  download via `DownloadPath`).

### Rust Behavior
- **Flat single-level parallelism**: all parts across all tables are flattened
  into a single `Vec<DownloadWorkItem>` and dispatched via one `tokio::Semaphore`.
- No separate metadata-download phase -- manifest is fetched once, then all
  parts are processed in a single parallel sweep.
- Only supports archive mode (tar+compression). There is no directory format
  (`DirectoryFormat`) download path.

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D1 | **No `download_by_part` / recursive incremental chain download** | HIGH | Go's `download_by_part` (default `true`) enables downloading only the parts that changed. When `download_by_part=false` and `RequiredBackup != ""`, Go recursively downloads the entire base backup first, then the incremental. Rust has no equivalent -- it downloads whatever parts are listed in the manifest without chasing the incremental chain. This means incremental backups that rely on a base backup's parts being present locally may fail at restore time. |
| D2 | **No `DirectoryFormat` (uncompressed directory) download path** | MEDIUM | Go supports `DataFormat == "directory"` where parts are stored as individual files in S3 (not tar archives). Rust only handles archive format. If a backup was created in directory format by Go, Rust cannot download it. |
| D3 | **No table-pattern filtering on download** | LOW | Go accepts `tablePattern` and `partitions` parameters, filtering which tables/parts to download. Rust downloads all tables from the manifest unconditionally. |

---

## 2. Decompression (LZ4 + untar)

### Go Behavior
- Uses the `mholt/archiver` library with pluggable compression (tar.gz,
  tar.lz4, tar.zstd, etc.).
- Streams decompression: `GetFileReader` -> buffered NIO reader -> archiver
  `Extract` callback per entry.
- Detects compression format mismatch between file extension and config, falls
  back to extension-based detection with a warning.
- Context-aware extraction: each `io.Copy` checks `ctx.Done()` for cancellation.

### Rust Behavior
- Downloads full object to memory (`s3.get_object`), then decompresses
  synchronously in `spawn_blocking`.
- Supports lz4, zstd, gzip, and `none` (raw tar).
- No streaming -- entire compressed payload is buffered in memory before
  decompression begins.

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D4 | **No streaming download+decompress** | MEDIUM | Go streams decompression from the S3 reader. Rust buffers the entire compressed object in memory before decompressing. For very large parts (multiple GB), this increases peak memory usage. Acceptable for now per CLAUDE.md ("Phase 2a buffers compressed data") but diverges from Go's streaming approach. |
| D5 | **No compression format auto-detection from file extension** | LOW | Go falls back to the file extension when the config format doesn't match the actual archive extension. Rust trusts `manifest.data_format` unconditionally. |

---

## 3. CRC Verification

### Go Behavior
- Checksum verification happens in `hardlinkIfLocalPartExistsAndChecksumEqual`:
  `common.CalculateChecksum(existingPartPath, "checksums.txt")` computes
  CRC64-ECMA of the `checksums.txt` file and compares against
  `table.Checksums[part.Name]`.
- The checksum map (`table.Checksums`) is stored per-table in the metadata, not
  per-part in the manifest. It maps `part_name -> crc64_value`.
- **No post-download CRC verification on fresh downloads** -- Go only verifies
  checksums when doing hardlink dedup. The fresh download path trusts the
  archiver extraction.

### Rust Behavior
- Post-download CRC64 verification on every local disk part: after
  decompressing, computes CRC64 of `checksums.txt` and compares against
  `part.checksum_crc64` from the manifest.
- On mismatch: deletes corrupted directory, retries up to
  `retries_on_failure` times with jittered delay.
- Also uses CRC64 in hardlink dedup (`find_existing_part`).

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D6 | **Rust is stricter than Go on CRC verification** | INFO | Rust verifies CRC64 after every download+decompress, while Go only checks during hardlink dedup. This is not a bug -- Rust is more defensive. No action needed. |

---

## 4. Hardlink Dedup (`--hardlink-exists-files`)

### Go Behavior
- `hardlinkIfLocalPartExistsAndChecksumEqual` searches three locations for
  existing parts:
  1. `{disk.Path}/data/{db}/{table}/{part_name}` (active data directory)
  2. `{disk.Path}/store/{uuid[:3]}/{uuid}/{part_name}` (UUID-based store path)
  3. `{disk.Path}/backup/*/shadow/{db}/{table}/{disk_name}/{part_name}` (other
     backup shadow dirs, via `filepath.Glob`)
- Iterates over ALL disks of the same type (not just the target disk).
- On hardlink failure where both source and dest `os.SameFile` is true, silently
  continues (file already linked).
- Sets `Chmod(0640)` on hardlinked files.
- Supports disk rebalancing: if the part is found on a different disk, sets
  `part.RebalancedDisk`.

### Rust Behavior
- `find_existing_part` only searches one location:
  - `{data_path}/backup/*/shadow/{db}/{table}/{part_name}` (other backup
    shadow dirs)
- Does NOT search the active data directory or UUID-based store paths.
- Does NOT iterate over multiple disks -- only searches the single `data_path`.
- No `SameFile` check on hardlink error.
- No explicit `chmod(0640)` after hardlink.
- No disk rebalancing support.

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D7 | **Missing active data dir and UUID store path search for hardlink dedup** | MEDIUM | Go searches `{disk}/data/{db}/{table}/{part}` and `{disk}/store/{uuid[:3]}/{uuid}/{part}` in addition to backup shadows. Rust only searches backup shadow dirs. This means Rust misses dedup opportunities when a part already exists in the active ClickHouse data directory. |
| D8 | **Single-disk hardlink search vs multi-disk** | LOW | Go iterates all disks of the same type. Rust only searches under `data_path`. Multi-disk setups may miss dedup candidates. |
| D9 | **No chmod(0640) after hardlink** | LOW | Go explicitly sets `0640` permissions on hardlinked files. Rust preserves source permissions. |
| D10 | **No disk rebalancing during dedup** | LOW | Go sets `part.RebalancedDisk` when a dedup candidate is found on a different disk. Rust has no rebalancing concept. |

---

## 5. S3 Disk Part Handling

### Go Behavior
- S3 disk parts are handled through `filterPartsAndFilesByDisk` which filters
  out disks matching `skip_disks` / `skip_disk_types`.
- For object disk parts, the data stays on S3 and only metadata is managed.
- Disk rebalancing (`reBalanceTablesMetadataIfDiskNotExists`) handles the case
  where the destination server has different disk names/policies than the source.

### Rust Behavior
- S3 disk parts detected via `is_s3_disk(disk_type) && part.s3_objects.is_some()`.
- Downloads only metadata files (S3 object references), actual data stays on S3.
- No disk rebalancing.

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D11 | **No disk rebalancing (`reBalanceTablesMetadataIfDiskNotExists`)** | MEDIUM | Go redistributes parts across available disks when the backup references a disk that doesn't exist on the destination server (e.g., migration to a different storage topology). Rust does not handle this case -- download would succeed but restore might fail due to disk path mismatch. |

---

## 6. Resume Support

### Go Behavior
- Uses BoltDB (`download.state2`) for persistent resume state.
- `resumableState.IsAlreadyProcessed(key)` returns `(bool, int64)` -- both
  processed flag and stored size.
- `adjustResumeFlag` auto-enables resume when `UseResumableState` config is true
  (regardless of CLI flag).
- Validates parameters via `cleanupStateIfParamsChange` using map comparison.
- Checks for existing local backup + state file to determine if resume is valid.

### Rust Behavior
- Uses JSON file (`download.state.json`) with atomic write (`.tmp` + rename).
- `DownloadState` with `completed_keys: HashSet<String>` and `params_hash`.
- Resume gated by both `--resume` CLI flag AND `config.general.use_resumable_state`.
- Validates params via `compute_params_hash`.
- State deleted on successful completion.

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D12 | **Resume auto-enable behavior differs** | LOW | Go auto-enables resume when `UseResumableState` is set in config, regardless of CLI flag. Rust requires both the CLI flag and config. Minor behavioral difference. |
| D13 | **No existing-backup check before download** | MEDIUM | Go checks if the backup already exists locally and either returns `ErrBackupIsAlreadyExists` or proceeds with resume. Rust does not check for existing local backups before downloading, potentially overwriting an existing local backup without warning. |

---

## 7. Disk Space Pre-Flight

### Go Behavior
- `CheckDisksUsage`: sums `FreeSpace` across all disks, compares against
  `max(backup.CompressedSize, backup.DataSize)`.
- Skips check when `tablePattern` is set (partial download).
- Skips check when `hardlinkExistsFiles` is true.
- On insufficient space with resume: warns but continues.
- On insufficient space without resume: returns error.

### Rust Behavior
- Uses `statvfs` on the backup directory's parent path.
- Applies 5% safety margin.
- Always checks (no skip for table pattern or hardlink mode).
- On `statvfs` failure: warns and continues.

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D14 | **No hardlink-mode bypass for disk space check** | LOW | Go skips the disk space check when `hardlinkExistsFiles` is true (since hardlinks don't consume additional space). Rust always checks. This could cause false-positive "insufficient space" errors when using hardlink dedup. |
| D15 | **Single-disk check vs multi-disk aggregation** | LOW | Go sums free space across all ClickHouse disks. Rust only checks the single `data_path` filesystem. Multi-disk setups may get inaccurate space estimates. |

---

## 8. Error Handling and Retry

### Go Behavior
- Uses `retrier.New(retrier.ExponentialBackoff(...))` with configurable
  `RetriesOnFailure`, `RetriesDuration`, and `RetriesJitter`.
- Retry wraps individual file downloads (`GetFileReader`, `DownloadCompressedStream`).
- `errgroup.WithContext` provides fail-fast behavior.
- Errors wrapped with `pkg/errors` for stack traces.
- Context cancellation propagated through all download operations.

### Rust Behavior
- Custom retry loop with `effective_retries`, `effective_retry_delay_secs`,
  and `apply_jitter`.
- Retry wraps the full download+decompress+CRC cycle (not just the download).
- `try_join_all` provides fail-fast behavior.
- `anyhow::Context` for error chain.
- No explicit context/cancellation propagation (relies on tokio task abort).

### Gaps
| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D16 | **Retry scope differs** | LOW | Go retries the individual S3 read operation. Rust retries the full download+decompress+CRC verify cycle. Rust's approach is simpler but re-downloads on decompression failures even when the download itself succeeded. |
| D17 | **No explicit cancellation propagation** | LOW | Go uses `ctx.Done()` checks during extraction. Rust relies on tokio's task abort mechanism. Both achieve cancellation but through different mechanisms. |

---

## 9. Additional Go Features Missing in Rust

| # | Gap | Severity | Notes |
|---|-----|----------|-------|
| D18 | **No RBAC/config/named-collections download flags** | LOW | Go has `--rbac-only`, `--configs-only`, `--named-collections-only` flags. Rust downloads RBAC and configs automatically but has no flags to download them exclusively. |
| D19 | **No `.inner.` table auto-discovery** | MEDIUM | Go's `downloadMissedInnerTablesMetadata` detects materialized views and automatically downloads their inner storage tables (`.inner.{table}` or `.inner_id.{uuid}`). Rust does not handle this case -- inner tables must be explicitly included in the table pattern. |
| D20 | **No embedded backup support** | LOW | Go supports ClickHouse's native embedded backup format (`BACKUP ... TO Disk`). Rust does not. This is a niche feature used primarily with ClickHouse 23.3+ embedded backup disks. |
| D21 | **No `chown` after download** | MEDIUM | Go calls `filesystemhelper.Chown()` on downloaded files to set ClickHouse uid/gid ownership. Rust download does not chown. Files are left with the process owner's permissions. This can cause permission errors when ClickHouse tries to read backup data during restore. |
| D22 | **No post-download cleanup of partial required backup** | LOW | Go's `cleanPartialRequiredBackup` removes partially downloaded base backups after the incremental is fully downloaded. Rust has no equivalent. |
| D23 | **No `should_skip_by_table_engine` / `should_skip_by_table_name` filtering** | LOW | Go filters out certain table engines and table name patterns during metadata download. Rust processes all tables from the manifest. |

---

## Priority Summary

### HIGH Priority (functional gaps that affect correctness)
- **D1**: No `download_by_part` / recursive incremental chain download

### MEDIUM Priority (behavioral gaps that affect specific use cases)
- **D2**: No `DirectoryFormat` download path
- **D7**: Missing active data dir and UUID store search for hardlink dedup
- **D11**: No disk rebalancing for cross-server migration
- **D13**: No existing-backup check before download
- **D19**: No `.inner.` table auto-discovery for materialized views
- **D21**: No `chown` after download

### LOW Priority (minor behavioral differences or edge cases)
- D3, D5, D8, D9, D10, D12, D14, D15, D16, D17, D18, D20, D22, D23

### INFO (Rust is more defensive -- no action needed)
- D4 (streaming is a nice-to-have optimization, not a correctness issue)
- D6 (Rust is stricter on CRC verification -- better than Go)

---

## Files Examined

### Go (Altinity/clickhouse-backup)
- `pkg/backup/download.go` -- main download orchestration
- `pkg/backup/backuper.go` -- `CheckDisksUsage`, `adjustResumeFlag`, `filterPartsAndFilesByDisk`
- `pkg/common/common.go` -- `CalculateChecksum` (CRC64-ECMA)
- `pkg/storage/general.go` -- `DownloadCompressedStream`, `DownloadPath`
- `pkg/resumable/state.go` -- BoltDB resume state
- `pkg/config/config.go` -- `DownloadByPart` default `true`

### Rust (chbackup)
- `/Users/rafael.siqueira/dev/personal/chbackup/src/download/mod.rs` -- main download flow
- `/Users/rafael.siqueira/dev/personal/chbackup/src/download/stream.rs` -- decompression (lz4, zstd, gzip, none)
- `/Users/rafael.siqueira/dev/personal/chbackup/src/resume.rs` -- resume state types and helpers
- `/Users/rafael.siqueira/dev/personal/chbackup/src/config.rs` -- download-related config fields
- `/Users/rafael.siqueira/dev/personal/chbackup/src/main.rs` -- download command dispatch (lines 215-238)
