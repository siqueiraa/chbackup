# Handoff: Per-Disk Backup Directories

## Plan Location
`docs/plans/2026-02-20-02-per-disk-backup-dirs/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 11 tasks across 5 dependency groups, TDD steps for each |
| SESSION.md | Status tracking and phase checklists |
| acceptance.json | 12 criteria (F001-F011 + FDOC) with 4-layer verification |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Per-disk backup directory layout patterns |
| context/symbols.md | Type verification table for all key types |
| context/diagnostics.md | Clean cargo check baseline (0 errors, 0 warnings) |
| context/references.md | Call hierarchy and reference analysis for all affected functions |
| context/git-history.md | Recent git log and file-specific history |
| context/knowledge_graph.json | Structured JSON with verified symbol imports and locations |
| context/affected-modules.json | Machine-readable module status (4 modules to update) |
| context/affected-modules.md | Human-readable affected modules summary |
| context/redundancy-analysis.md | No new public API; extending existing functions only |
| context/data-authority.md | All data already available in existing codebase |
| context/preventive-rules-applied.md | RC-002, RC-006, RC-008, RC-015, RC-019, RC-021 applied |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Problem Summary

On multi-NVMe ClickHouse setups, `collect_parts()` hardlinks all parts to `{data_path}/backup/{name}/shadow/` regardless of source disk. Parts on non-default NVMe disks cross filesystem boundaries, causing EXDEV -> full copy fallback (dramatically slower than hardlink).

**Fix:** Stage each disk's parts to `{disk_path}/backup/{name}/shadow/...` so hardlinks stay on the same filesystem. Update all consumers (upload, download, restore, delete) to resolve per-disk paths via `BackupManifest.disks`.

**Key invariant:** Single-disk setups (disk_path == data_path) produce identical directory layout. Zero behavior change.

## Key References

### Files Being Modified
- `src/backup/collect.rs` - per_disk_backup_dir() helper, collect_parts() staging_dir change
- `src/backup/mod.rs` - collect_parts() caller update, error cleanup per-disk dirs
- `src/upload/mod.rs` - find_part_dir() per-disk resolution, delete_local per-disk cleanup
- `src/download/mod.rs` - per-disk download target, find_existing_part() per-disk search
- `src/restore/attach.rs` - OwnedAttachParams.manifest_disks, attach_parts_inner per-disk source
- `src/restore/mod.rs` - ATTACH TABLE mode per-disk shadow path, manifest_disks wiring
- `src/list.rs` - delete_local() per-disk cleanup via manifest loading

### Test Files
- Tests are added inline in each source file (Rust convention for unit tests)
- `src/backup/collect.rs` - test_per_disk_backup_dir_*, test_collect_parts_per_disk_*
- `src/upload/mod.rs` - test_find_part_dir_per_disk, test_find_part_dir_fallback_default
- `src/download/mod.rs` - test_download_per_disk_*, test_find_existing_part_per_disk
- `src/restore/attach.rs` - test_attach_source_dir_per_disk
- `src/restore/mod.rs` - test_attach_table_mode_per_disk
- `src/list.rs` - test_delete_local_cleans_per_disk_dirs, test_delete_local_no_manifest_fallback
- `src/backup/mod.rs` - test_create_error_cleanup_per_disk

### Design Doc References
- No design doc changes needed -- per-disk layout is an implementation optimization
- BackupManifest.disks (manifest.rs) already documents the disk->path mapping
- Hardlink with EXDEV fallback pattern documented in CLAUDE.md Key Implementation Patterns

### Critical Code Paths (from context/references.md)
- `collect_parts()` staging_dir: line ~303 of collect.rs (THE key change)
- `find_part_dir()`: line ~1065 of upload/mod.rs (upload consumer)
- `attach_parts_inner()` source_dir: line ~519 of attach.rs (restore consumer)
- `restore_s3_disk_parts()` source_dir: line ~293 of attach.rs (S3 metadata read)
- `try_attach_table_mode()` shadow_base: line ~987 of restore/mod.rs (ATTACH TABLE mode)
- `delete_local()`: line ~477 of list.rs (cleanup consumer)
- `upload()` delete_local: line ~989 of upload/mod.rs (post-upload cleanup)
- `create()` error cleanup: line ~594 of backup/mod.rs (failure cleanup)
