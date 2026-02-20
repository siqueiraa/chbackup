# Handoff: Phase 4e -- RBAC & Config Backup/Restore

## Plan Location
`docs/plans/2026-02-19-02-phase4e-rbac-config-backup/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (6 tasks, 3 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 7 criteria with 4-layer verification |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns (10 patterns identified) |
| context/diagnostics.md | Compiler baseline (0 errors, 0 warnings) |
| context/symbols.md | Type verification table |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | 6 modules to update, 0 to create |
| context/affected-modules.md | Human-readable module summary |
| context/redundancy-analysis.md | No REPLACE decisions needed |
| context/references.md | Symbol and reference analysis |
| context/git-history.md | Recent git context |
| context/preventive-rules-applied.md | Applied 14 preventive rules |
| context/data-authority.md | Data source authority analysis |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/clickhouse/client.rs` -- Add query_rbac_objects() and query_named_collections() methods
- `src/backup/rbac.rs` (NEW) -- RBAC/config/named-collections backup logic
- `src/backup/mod.rs` -- Wire backup_rbac_and_configs() into create() flow, add 3 params
- `src/upload/mod.rs` -- Add upload_simple_directory() for access/ and configs/
- `src/download/mod.rs` -- Add download_simple_directory() for access/ and configs/
- `src/restore/rbac.rs` (NEW) -- RBAC/config/named-collections restore + restart_command
- `src/restore/mod.rs` -- Wire Phase 4 extensions, add 3 params to restore()
- `src/main.rs` -- Remove 12 "not yet implemented" warnings, pass flags through
- `src/server/routes.rs` -- Add rbac/configs/named_collections to 4 request types
- `src/server/state.rs` -- Update auto_resume call (pass false, false, false)
- `src/watch/mod.rs` -- Update watch loop create() call (pass false, false, false)

### Callers That Need Updating (backup::create)
1. `src/main.rs:154` -- Command::Create
2. `src/main.rs:308` -- Command::CreateRemote
3. `src/server/routes.rs:318` -- create_backup handler
4. `src/server/routes.rs:652` -- create_remote handler
5. `src/watch/mod.rs:412` -- watch loop

### Callers That Need Updating (restore::restore)
1. `src/main.rs:260` -- Command::Restore
2. `src/main.rs:380` -- Command::RestoreRemote
3. `src/server/routes.rs:566` -- restore_backup handler
4. `src/server/routes.rs:804` -- restore_remote handler
5. `src/server/state.rs:386` -- auto_resume handler

### Design Doc Sections
- Section 3.4 (step 4) -- Backup RBAC, configs, named collections
- Section 3.6 (step 5) -- Upload RBAC/named-collections metadata
- Section 5.6 -- Phase 4: Functions, Named Collections, RBAC restore
- Section 7.1 -- Manifest format (rbac, named_collections fields)
- Section 12 -- Config fields (restart_command, *_backup_always, rbac_resolve_conflicts)

### Related Documentation
- `src/backup/CLAUDE.md` -- Backup module patterns
- `src/restore/CLAUDE.md` -- Restore module patterns, Phase 4 architecture
- `src/clickhouse/CLAUDE.md` -- ChClient methods and Row types
- `src/upload/CLAUDE.md` -- Upload pipeline patterns
- `src/download/CLAUDE.md` -- Download pipeline patterns

## Key Patterns to Follow

1. **create_functions() in restore/schema.rs:721-755** -- Template for restore_named_collections(). Sequential DDL, non-fatal failures, ON CLUSTER.
2. **list_tables() in clickhouse/client.rs:262** -- Template for new ChClient query methods. Row struct + fetch_all + context.
3. **spawn_blocking for sync I/O** -- All filesystem operations use this pattern (walkdir, fs::copy).
4. **detect_clickhouse_ownership() + chown** -- Reuse from restore/attach.rs:713 for RBAC file chown.
5. **put_object/get_object for simple files** -- No compression needed for RBAC/config files.
