# Preventive Rules Applied

## Rules Checked

| Rule | Applicable | Notes |
|------|------------|-------|
| RC-001 | NO | No actors in this project (not kameo-based) |
| RC-002 | YES | Must verify actual types before using in plan code |
| RC-003 | YES | Track all files after implementation |
| RC-004 | NO | No actor message system |
| RC-005 | YES | Check division in disk space calculations |
| RC-006 | YES | Verify every API/method exists before code snippets |
| RC-007 | YES | Verify tuple/struct field order |
| RC-008 | YES | TDD sequencing -- fields must exist or be added in preceding task |
| RC-010 | NO | No adapters/actors |
| RC-011 | YES | State file flags: resume state must clear on success/error/timeout |
| RC-015 | YES | Cross-task return type verification |
| RC-016 | YES | Struct field completeness for consumer tasks |
| RC-017 | YES | State field declarations verified |
| RC-018 | YES | Every task needs explicit test steps |
| RC-019 | YES | Follow existing patterns for similar code |
| RC-021 | YES | Verify struct/field file locations |
| RC-032 | YES | Verify data source authority before adding tracking |

## Applied Checks

### RC-002: Schema/Type Verification
- All config fields verified via `src/config.rs` read
- `ClickHouseConfig` TLS fields: `secure: bool`, `skip_verify: bool`, `tls_key: String`, `tls_cert: String`, `tls_ca: String` -- already defined
- `skip_disks: Vec<String>`, `skip_disk_types: Vec<String>` -- already defined in config
- `check_parts_columns: bool` -- already defined in config
- `use_resumable_state: bool` -- already defined in config

### RC-005: Division Checks
- Disk space check: `required_space = total - hardlink_savings`. Must handle case where `actual_free` is 0 and avoid dividing by 0 in percentage calculations.

### RC-006: API Verification
- `BackupManifest::save_to_file`, `load_from_file`, `from_json_bytes`, `to_json_bytes` -- all verified in `src/manifest.rs`
- `ChClient::freeze_table`, `unfreeze_table`, `list_tables`, `get_disks`, `execute_ddl`, `attach_part` -- verified in `src/clickhouse/client.rs`
- `S3Client::put_object`, `get_object`, `copy_object`, `copy_object_with_retry`, `list_objects`, `delete_object` -- verified in `src/storage/s3.rs`
- `compute_crc64`, `compute_crc64_bytes` -- verified in `src/backup/checksum.rs`
- `is_s3_disk` -- verified in `src/object_disk.rs`
- `effective_upload_concurrency`, `effective_download_concurrency`, `effective_max_connections` -- verified in `src/concurrency.rs`

### RC-011: State Flag Exit Paths
- Resume state files (`upload.state.json`, `download.state.json`, `restore.state.json`) must be:
  1. Created at operation start
  2. Updated after each part completion
  3. Deleted on successful operation completion
  4. Left intact on failure (for resume)
  5. Write failures: warn, never fatal (per design 16.1)

### RC-019: Follow Existing Patterns
- Upload pipeline pattern: flat semaphore + `try_join_all` + rate limiter. Resume state should integrate into this existing pattern.
- Download pipeline pattern: same architecture. CRC64 verification should follow existing `compute_crc64_bytes` pattern.
- List pattern: `BackupSummary` already has `is_broken: bool` field. Extend to show `[BROKEN]` marker.

### RC-021: File Location Verification
- Config fields: ALL Phase 2d fields already exist in `src/config.rs`
  - `clickhouse.secure`, `skip_verify`, `tls_key`, `tls_cert`, `tls_ca` -- lines 118-135
  - `clickhouse.skip_disks`, `skip_disk_types` -- lines 220-226
  - `clickhouse.check_parts_columns` -- line 147
  - `general.use_resumable_state` -- line 91
- CLI flags: `--resume` already defined on Create, Upload, Download, Restore in `src/cli.rs`
- `BackupSummary.is_broken` -- already in `src/list.rs` line 37

### RC-032: Data Authority
- See `context/data-authority.md` for detailed analysis
