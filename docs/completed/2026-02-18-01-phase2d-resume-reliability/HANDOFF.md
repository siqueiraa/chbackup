# Handoff: Phase 2d -- Resume & Reliability

## Plan Location
`docs/plans/2026-02-18-01-phase2d-resume-reliability/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 12 task definitions with TDD steps, dependency groups, consistency validation |
| SESSION.md | Status tracking, planning/execution phase checklists |
| acceptance.json | 12 criteria with 4-layer verification (structural, compilation, behavioral, runtime) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | 9 discovered patterns (existing + new Phase 2d patterns) |
| context/symbols.md | Type verification table with 56 verified types/functions |
| context/knowledge_graph.json | Structured JSON for symbol lookup (verified imports) |
| context/affected-modules.json | Machine-readable module status (6 modules to update) |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Compiler state (clean, zero warnings) |
| context/references.md | Symbol references and cross-module callers |
| context/git-history.md | Git log showing Phase 2c completion as latest |
| context/redundancy-analysis.md | New component analysis (EXTEND, COEXIST, REUSE decisions) |
| context/preventive-rules-applied.md | 17 RC rules checked with application notes |
| context/data-authority.md | Data source verification for new tracking fields |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/clickhouse/client.rs` -- TLS support, new query methods (freeze_partition, query_system_parts, check_parts_columns, query_disk_free_space)
- `src/upload/mod.rs` -- Resume state tracking, manifest atomicity (.tmp + CopyObject + delete)
- `src/download/mod.rs` -- Resume state, CRC64 post-download verification, disk space pre-flight
- `src/restore/mod.rs` -- Resume state, system.parts query for already-attached
- `src/restore/attach.rs` -- Skip already-attached parts
- `src/backup/mod.rs` -- Partition-level freeze routing, disk filtering, parts column check
- `src/backup/freeze.rs` -- Partition-level FREEZE PARTITION support
- `src/list.rs` -- Broken reason display, clean_broken_local/remote implementation
- `src/table_filter.rs` -- is_disk_excluded() function
- `src/main.rs` -- Wire --resume, --partitions flags, clean_broken dispatch
- `src/error.rs` -- Possibly add new error variants

### New Files
- `src/resume.rs` -- Shared resume state types (UploadState, DownloadState, RestoreState), load/save/delete helpers

### Test Files
- Unit tests in each module's `#[cfg(test)] mod tests` block
- Integration tests require real ClickHouse + S3 (not included in plan)

### Design Doc References
- Section 3.3: Parts column consistency check
- Section 3.4: FREEZE and Collect (partition-level freeze)
- Section 3.6: Upload (resume, manifest atomicity)
- Section 4: Download (resume, CRC64 verification, disk space)
- Section 5.3: Restore (resume, system.parts query)
- Section 8.4: Broken backup cleanup
- Section 12: Configuration (TLS, skip_disks, skip_disk_types)
- Section 16.1: State degradation (warn, never fatal)
- Section 16.3: Disk space pre-flight check

### Key Architectural Decisions
1. Resume state files are JSON, written atomically (write .tmp, rename)
2. State degradation: all state file writes are non-fatal (warn on error, continue)
3. Manifest atomicity: upload to .tmp key, CopyObject to final, delete .tmp
4. Disk space check uses `nix::sys::statvfs` (not ClickHouse system.disks) because download() doesn't take ChClient
5. TLS support via clickhouse-rs crate's native configuration; env vars as fallback
6. New `src/resume.rs` module for shared types; no other new modules needed
