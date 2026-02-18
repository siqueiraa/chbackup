# Handoff: Phase 3c -- Retention / GC

## Plan Location
`docs/plans/2026-02-18-03-phase3c-retention-gc/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps for 7 tasks across 4 dependency groups |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (7 features: F001-F006 + FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns (list+filter+delete, route handler, CLI dispatch) |
| context/symbols.md | Type verification table for all types used |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Machine-readable module status |
| context/affected-modules.md | Human-readable module summary |
| context/data-authority.md | Data source analysis (all USE EXISTING) |
| context/diagnostics.md | Compiler baseline (0 errors, 0 warnings) |
| context/redundancy-analysis.md | New vs existing component analysis (all COEXIST) |
| context/references.md | Reference and call hierarchy analysis |
| context/git-history.md | Recent git log and file history |
| context/preventive-rules-applied.md | Applied preventive rules checklist |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Design Doc Sections
- `docs/design.md` section 8.1 -- Deleting local backups
- `docs/design.md` section 8.2 -- Safe GC for remote backups (the GC algorithm)
- `docs/design.md` section 8.3 -- Auto-retention config semantics
- `docs/design.md` section 8.4 -- Broken backup cleanup (already implemented)
- `docs/design.md` section 13 -- Clean command (shadow directory cleanup)

### Files Being Modified
- `src/list.rs` -- All retention/GC/clean_shadow functions (Tasks 1-3, 6)
- `src/main.rs` -- Wire Command::Clean to clean_shadow (Task 4)
- `src/server/routes.rs` -- Replace clean_stub with real handler (Task 5)
- `src/server/mod.rs` -- Update route wiring from clean_stub to clean (Task 5)

### Files NOT Modified (already correct)
- `src/config.rs` -- RetentionConfig already has both fields at line 378
- `src/lock.rs` -- lock_for_command("clean") already returns LockScope::Global
- `src/error.rs` -- Using anyhow::Result throughout, no new variants needed
- `src/cli.rs` -- Command::Clean already defined with --name optional arg

### Test Files
- `src/list.rs` (inline #[cfg(test)] module) -- Unit tests for retention_local, collect_keys_from_manifest, gc_filter, clean_shadow_dir

### Key Patterns to Follow
- `clean_broken_local()` at list.rs:292 -- Template for retention_local (list -> filter -> delete loop -> count)
- `clean_broken_remote()` at list.rs:325 -- Template for retention_remote
- `clean_remote_broken` handler at routes.rs:860 -- Template for /api/v1/clean handler
- `Command::CleanBroken` at main.rs:374 -- Template for Command::Clean dispatch

### Important Type Details
- `BackupSummary.timestamp` is `Option<DateTime<Utc>>` -- broken backups have None
- `RetentionConfig.backups_to_keep_local` is `i32` -- 0=unlimited, -1=delete after upload, N>0=keep N
- `PartInfo.backup_key` is `String` -- relative key like "backup_name/data/db/table/disk/part.tar.lz4"
- `PartInfo.s3_objects` is `Option<Vec<S3ObjectInfo>>` -- each S3ObjectInfo also has a backup_key
- `DiskRow.disk_type` uses `#[serde(rename = "type")]` -- access as `disk.disk_type` not `disk.type_field`
