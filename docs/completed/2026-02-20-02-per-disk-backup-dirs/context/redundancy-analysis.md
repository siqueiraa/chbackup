# Redundancy Analysis

## New Public API Introduced

This plan does NOT introduce new public structs, enums, or modules. It modifies existing functions:

1. **collect_parts()** -- Modified signature (add `backup_name: &str` parameter) or change internal staging logic
2. **find_part_dir()** -- Modified to search per-disk backup dirs
3. **delete_local()** -- Modified to clean per-disk backup dirs
4. Potentially a new helper function: `per_disk_backup_dir(disk_path, backup_name)` or `resolve_part_dir_for_disk(manifest, disk_name, backup_name)`

## Helper Function Search

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| `per_disk_backup_dir()` helper | None found | N/A -- new internal helper | No equivalent exists; computes `{disk_path}/backup/{backup_name}/shadow/` |
| `delete_per_disk_dirs()` helper | `delete_local()` in list.rs | EXTEND | Extend delete_local to also remove per-disk dirs using manifest.disks |
| `find_part_dir_per_disk()` | `find_part_dir()` in upload/mod.rs | EXTEND | Extend existing function to check multiple disk backup dirs |

## Conclusion

N/A -- no new public API introduced. Modifying existing code only. Internal helpers may be added but they will be private to their modules.
