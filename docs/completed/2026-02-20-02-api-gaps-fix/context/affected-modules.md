# Affected Modules Analysis

## Summary

- **Module directories to update:** 1 (src/server)
- **Standalone files to modify:** 3 (src/list.rs, CLAUDE.md, docs/design.md)
- **Module directories to create:** 0
- **Git base:** 3a6e946d

## Modules Being Modified

| Module/File | CLAUDE.md Status | Triggers | Action |
|-------------|------------------|----------|--------|
| src/server | EXISTS | bug_fix, new_feature | UPDATE |
| src/list.rs | N/A (standalone file) | field_additions | MODIFY |
| CLAUDE.md | N/A (root doc) | doc_fix | MODIFY |
| docs/design.md | N/A (design doc) | doc_fix | MODIFY |

## Files Modified Per Issue

### BUG-1 (post_actions stub)
- `src/server/routes.rs` -- Replace stub with actual dispatch

### MISSING-1 (list pagination/format)
- `src/server/routes.rs` -- Add offset/limit/format to ListParams, change list_backups return type
- `src/list.rs` -- No change needed (ListFormat and format_list_output already exist)

### MISSING-2 (required field)
- `src/list.rs` -- Add `required` field to BackupSummary, extract in parse_backup_summary and list_remote
- `src/server/routes.rs` -- Wire BackupSummary.required to ListResponse.required in summary_to_list_response

### MISSING-3 (SIGTERM handler)
- `src/server/mod.rs` -- Add SIGTERM handler alongside SIGINT

### MISSING-4 (object_disk_size)
- `src/list.rs` -- Add `object_disk_size` field to BackupSummary, compute from s3_objects
- `src/server/routes.rs` -- Wire BackupSummary.object_disk_size to ListResponse.object_disk_size in summary_to_list_response

### DOC fixes
- `CLAUDE.md` -- Fix GeneralConfig param count, WatchConfig param count, remove phantom param reference, fix Phase 6 note
- `docs/design.md` -- Fix section 7.1 JSON example, fix MutationInfo.parts_to_do type

## CLAUDE.md Tasks to Generate

1. **Update:** src/server/CLAUDE.md
   - Document list endpoint pagination (offset, limit, format, X-Total-Count)
   - Document post_actions actual dispatch behavior
   - Document SIGTERM handler
   - Update ListParams documentation
