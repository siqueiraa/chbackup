# Data Authority Analysis

## Item 7: API list response sizes

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| metadata_size | BackupManifest | `metadata_size: u64` (manifest.rs:48) | USE EXISTING |
| rbac_size | BackupManifest | `rbac: Option<RbacInfo>` but no size field | MUST IMPLEMENT - sum JSONL file sizes from access/ dir |
| config_size | Backup directory | configs/ directory | MUST IMPLEMENT - sum file sizes from configs/ dir |
| object_disk_size | BackupManifest | Can be computed from `PartInfo.s3_objects` sizes | MUST IMPLEMENT - sum s3_objects sizes across all parts |

### Analysis Notes

1. **metadata_size**: The manifest already stores this field (`manifest.rs:48`). It is populated in `backup/mod.rs:614-619` after writing the manifest file. The `BackupSummary` struct does NOT currently expose this field, but the data is available in the manifest. The fix is to thread it through `BackupSummary`.

2. **rbac_size**: The `RbacInfo` struct only has a `path` field (e.g., "access/"). To compute the actual size, we would need to either (a) sum the JSONL files at backup time and store in manifest, or (b) estimate from the manifest. For now, leaving as 0 is acceptable since RBAC backup is not commonly used. A future enhancement could store `rbac_size` in `RbacInfo`.

3. **config_size**: Similar to rbac_size -- config file backup stores files in `configs/` but doesn't track total size. Leaving as 0 is acceptable for now.

4. **object_disk_size**: Could be computed from manifest by summing all `PartInfo.s3_objects` sizes where the disk type is s3. This is a pure computation from existing data but requires loading the full manifest. For the list endpoint which already loads manifests, this is feasible.

### Priority Decision

- **metadata_size**: Fix now (data already available, just not threaded through)
- **rbac_size**: Leave as 0 (requires manifest schema change or filesystem scan)
- **config_size**: Leave as 0 (same reason)
- **object_disk_size**: Consider implementing from manifest data

## Item 5: Progress Bar

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Total parts count | BackupManifest | Available in all command pipelines | USE EXISTING |
| Completed parts count | Per-pipeline counters | Currently logged but not tracked in a progress-visible way | MUST IMPLEMENT |
| Bytes progress | Rate limiter | Bytes consumed tracked per-part | MUST IMPLEMENT - aggregate counter |

### Analysis Notes

The design doc (11.4) specifies `indicatif` for progress bars. The crate is NOT currently in `Cargo.toml`. Adding progress bar support requires:
1. Adding `indicatif` dependency
2. Creating a `ProgressTracker` struct (design shows this at line ~2346)
3. Integrating into upload, download, create, restore pipelines
4. Respecting `disable_progress_bar` config and TTY detection

This is the most complex item in the plan and the least critical. The design mentions it but it's truly a polish feature.

## Items 3, 4: Skip-Projections and Hardlink-Exists-Files

These flags are pure control-flow additions to existing pipelines. No new data tracking needed -- they use existing manifest data (CRC64, part names) or filesystem state (projection subdirectories).
