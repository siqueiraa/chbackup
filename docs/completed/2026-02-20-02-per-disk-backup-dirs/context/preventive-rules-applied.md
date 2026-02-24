# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md`
- `.claude/skills/self-healing/references/planning-rules.md`

## Applicable Rules for This Plan

| Rule | Applicable? | How Applied |
|------|-------------|-------------|
| RC-002 (Type mismatch) | YES | Verified DiskRow fields: name, path, disk_type, remote_path -- all String |
| RC-006 (Unverified APIs) | YES | Verified: `hardlink_dir`, `collect_parts`, `find_part_dir`, `delete_local`, `dir_size`, `DiskRow` all exist in codebase |
| RC-007 (Tuple field order) | NO | No tuple types involved in this change |
| RC-008 (TDD sequencing) | YES | Will verify field/function dependencies between tasks |
| RC-015 (Cross-task return type) | YES | collect_parts returns HashMap<String, Vec<CollectedPart>> -- consumers must match |
| RC-016 (Struct field completeness) | YES | No new structs planned; modifying existing function signatures |
| RC-019 (Existing pattern) | YES | Must follow existing hardlink_dir/collect_parts patterns exactly |
| RC-021 (File location) | YES | Verified all struct/function locations with grep |
| RC-032 (Data authority) | LOW | Not adding tracking/calculation, just changing directory layout |

## Rules NOT Applicable
- RC-001/RC-004/RC-010/RC-020 (Actor/Kameo rules) -- No actors in this codebase
- RC-005 (Division by zero) -- No calculations involved
- RC-011 (State machine flags) -- No state flags
- RC-012/RC-013/RC-014 (E2E test rules) -- Not applicable
- RC-029 (Async signature change) -- No sync-to-async changes planned

## Key Verification Results

1. **hardlink_dir** -- exists at `src/backup/collect.rs:403`, signature: `fn hardlink_dir(src_dir: &Path, dst_dir: &Path, skip_proj_patterns: &[String]) -> Result<()>`
2. **collect_parts** -- exists at `src/backup/collect.rs:116`, takes `backup_dir: &Path` as 3rd parameter
3. **find_part_dir** -- exists at `src/upload/mod.rs:1065`, looks for parts at `{backup_dir}/shadow/{db}/{table}/{part_name}/`
4. **delete_local** -- exists at `src/list.rs:477`, calls `std::fs::remove_dir_all(&backup_dir)` on `{data_path}/backup/{backup_name}/`
5. **DiskRow** -- exists at `src/clickhouse/client.rs:52`, fields: name, path, disk_type, remote_path (all String)
6. **BackupManifest.disks** -- `HashMap<String, String>` at `src/manifest.rs:52` -- disk name -> disk path
