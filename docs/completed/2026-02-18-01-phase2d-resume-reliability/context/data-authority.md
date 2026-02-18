# Data Authority Analysis

## Data Requirements and Sources

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Completed upload parts | Self-tracked state | `upload.state.json` | MUST IMPLEMENT -- no external source; design 3.6 specifies state file tracking |
| Completed download parts | Self-tracked state | `download.state.json` | MUST IMPLEMENT -- no external source; design 4 specifies state file tracking |
| Attached restore parts | system.parts query | `system.parts WHERE name = X` | HYBRID -- use system.parts to verify on resume, plus `restore.state.json` for tracking |
| Part CRC64 checksum | Manifest `checksum_crc64` | `PartInfo.checksum_crc64: u64` | USE EXISTING -- manifest already stores CRC64 per part from create phase |
| Post-download CRC64 | Local file computation | `compute_crc64(checksums.txt)` | USE EXISTING -- `compute_crc64()` function already exists in `backup/checksum.rs` |
| Disk free space | system.disks query | `free_space` column | MUST IMPLEMENT -- need new ChClient query for `system.disks` free_space |
| Broken backup status | metadata.json presence | `BackupSummary.is_broken` | USE EXISTING -- `list.rs` already detects broken backups |
| Parts column consistency | system.parts_columns | grouped query | MUST IMPLEMENT -- need new ChClient query method |
| TLS config params | Config file | `ClickHouseConfig.secure/tls_*` | USE EXISTING -- all config fields already defined |
| Disk filtering config | Config file | `ClickHouseConfig.skip_disks/skip_disk_types` | USE EXISTING -- config fields already defined |
| Partition list | CLI `--partitions` flag | `String` (comma-separated) | MUST IMPLEMENT -- CLI flag exists but FREEZE PARTITION logic not implemented |
| Existing parts on table | ClickHouse system.parts | `name` column | MUST IMPLEMENT -- need ChClient query for system.parts by table |

## Analysis Notes

1. **Resume state is purely self-tracked**: There is no external system that tracks which parts have been uploaded or downloaded. The state files are the only source, and they must handle parameter change invalidation.

2. **Restore resume uses hybrid approach**: The state file tracks what we THINK is attached, but `system.parts` is the authoritative source for what is ACTUALLY attached. On resume, the design (5.3) says to query `system.parts` for already-attached parts.

3. **CRC64 is already computed at create time**: No need to re-compute during upload. The manifest carries `checksum_crc64` for each part. Post-download verification compares the locally-recomputed CRC64 against the manifest value.

4. **DiskRow already has all needed fields**: `name`, `path`, `disk_type`, `remote_path`. However, `free_space` is NOT yet queried -- need a new query or modify `get_disks()`.

5. **Broken backup detection is partially done**: `list_local()` and `list_remote()` already check for missing/corrupt manifests and set `is_broken = true`. What is missing: `[BROKEN]` display marker and `clean_broken` command implementation.

6. **All TLS and disk filtering config params already exist**: The config structure is complete. What is missing is the actual TLS usage in `ChClient::new()` (currently only uses `secure` for scheme selection) and disk filtering logic in `collect_parts()`.

## MUST IMPLEMENT Summary (with justification)

1. **Resume state files** -- No external data source exists for tracking operation progress across restarts
2. **system.disks free_space query** -- DiskRow struct needs `free_space` field or separate query
3. **system.parts_columns batch query** -- New query for column type consistency validation
4. **system.parts query by table** -- For restore resume to check already-attached parts
5. **FREEZE PARTITION SQL** -- New SQL variant for partition-level freeze
6. **Disk filtering in shadow walk** -- Check `skip_disks`/`skip_disk_types` during `collect_parts()`
7. **Manifest atomic upload** -- Change from direct PutObject to tmp+CopyObject+delete pattern
8. **clean_broken command** -- Implement the scan+delete logic for broken backups
9. **Post-download CRC64 verification** -- Add verification step after decompression with retry
10. **ClickHouse TLS certificate handling** -- Wire `tls_key`, `tls_cert`, `tls_ca` into `clickhouse::Client`
