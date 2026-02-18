# Redundancy Analysis

## New Public Components Proposed

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|----------------|----------|-----------------|---------------|
| `UploadState` struct | None | N/A - new | - | No existing resume state tracking |
| `DownloadState` struct | None | N/A - new | - | No existing resume state tracking |
| `RestoreState` struct | None | N/A - new | - | No existing resume state tracking |
| `save_resumable_state()` fn | None | N/A - new | - | State degradation pattern from design 16.1 |
| `load_resumable_state()` fn | None | N/A - new | - | Resume loading |
| `check_parts_columns()` ChClient method | None | N/A - new | - | No existing query for system.parts_columns |
| `get_disk_free_space()` ChClient method | `get_disks()` exists | EXTEND | - | Add free_space field to existing DiskRow struct or new method |
| `query_system_parts()` ChClient method | None | N/A - new | - | No existing query for system.parts by table/name |
| `freeze_partition()` fn | `freeze_table()` exists | COEXIST | - | Different SQL: FREEZE PARTITION vs FREEZE; both needed |
| `is_disk_excluded()` fn | `is_excluded()`, `is_engine_excluded()` exist | COEXIST | - | Different exclusion dimension (disk vs table vs engine) |
| `clean_broken_local()` fn | `list_local()` exists (detects broken) | EXTEND | - | list_local already detects broken; clean_broken adds deletion |
| `clean_broken_remote()` fn | `list_remote()` exists (detects broken) | EXTEND | - | list_remote already detects broken; clean_broken adds deletion |
| `verify_crc64_after_download()` fn | `compute_crc64_bytes()` exists | REUSE | - | Use existing CRC64 computation, add comparison+retry wrapper |

## Decision Details

### EXTEND: `get_disks()` -> add `free_space`
- Existing `DiskRow` has `name`, `path`, `disk_type`, `remote_path`
- Need to add `free_space: u64` field to the query/struct
- Alternative: separate query method `get_disk_free_space()` that queries `SELECT name, free_space FROM system.disks`
- Decision: Extend `DiskRow` to include `free_space` (optional field with default 0)

### COEXIST: `freeze_partition()` alongside `freeze_table()`
- `freeze_table()` issues `ALTER TABLE FREEZE WITH NAME`
- `freeze_partition()` issues `ALTER TABLE FREEZE PARTITION 'X' WITH NAME`
- Different SQL, different use cases. Both needed long-term.
- Cleanup deadline: N/A -- both are permanent API

### COEXIST: `is_disk_excluded()` alongside `is_excluded()`/`is_engine_excluded()`
- `is_excluded()`: checks table name against skip_tables patterns
- `is_engine_excluded()`: checks engine name against skip_table_engines
- `is_disk_excluded()`: checks disk name/type against skip_disks/skip_disk_types
- Different dimensions of filtering. All needed.
- Cleanup deadline: N/A -- all are permanent API

### REUSE: CRC64 verification
- `compute_crc64_bytes()` already exists and is correct
- Post-download verification just needs: compute CRC64 of downloaded checksums.txt, compare against manifest value
- No new CRC64 function needed -- only a comparison wrapper

### EXTEND: `list_local()`/`list_remote()` for clean_broken
- Both already detect broken backups and set `is_broken = true`
- `clean_broken` needs: call list, filter broken, delete each
- Can reuse the detection logic, just add the deletion step
