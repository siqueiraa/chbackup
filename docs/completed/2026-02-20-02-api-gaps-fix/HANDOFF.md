# Handoff: Fix Implementation Gaps Found in Audit of chbackup API

## Plan Location
`docs/plans/2026-02-20-02-api-gaps-fix/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (8 tasks in 5 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (8 criteria: F001-F007 + FDOC) |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Route handler delegation, signal handler, BackupSummary, pagination patterns |
| context/symbols.md | Type verification table (15 types verified) |
| context/diagnostics.md | Compiler state (clean), 5 bugs/gaps identified |
| context/knowledge_graph.json | Structured JSON for symbol lookup (20 verified symbols) |
| context/affected-modules.json | Machine-readable module status (1 module to update, 3 standalone files) |
| context/affected-modules.md | Human-readable module summary |
| context/data-authority.md | Data source verification (all USE EXISTING) |
| context/references.md | MCP references for key symbols (10 analyzed) |
| context/git-history.md | Git context (recent commits, file history) |
| context/preventive-rules-applied.md | Applied rules (RC-006, RC-008, RC-019, RC-021, RC-032) |
| context/redundancy-analysis.md | No new public API, coexist decision for helper function |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/server/routes.rs` - API endpoint handlers (post_actions dispatch, list pagination, ListResponse wiring)
- `src/server/mod.rs` - Server startup (SIGTERM handler)
- `src/list.rs` - BackupSummary struct (new fields: object_disk_size, required)
- `CLAUDE.md` - Root documentation (param count fixes, phantom reference removal)
- `docs/design.md` - Design doc (parts_to_do type fix, required_backups reference fix)

### Test Files
- `src/server/routes.rs` (bottom of file) - API tests (test_summary_to_list_response_sizes, test_tables_params_deserialization)
- `src/list.rs` (bottom of file) - List module tests (~12 BackupSummary construction sites)
- `src/lib.rs` (line 193) - Integration test BackupSummary site
- `src/watch/mod.rs` (line 722) - Watch module test helper

### Related Documentation
- `src/server/CLAUDE.md` - Server module documentation (updated in Task 8)
- `docs/design.md` - Design doc section 7.1 (manifest format) and 8.2 (retention)

### Key Patterns to Follow
- Route handler delegation: see `create_backup()` at routes.rs:342
- Pagination: see `tables()` at routes.rs:1495-1525
- Signal handler: see SIGHUP at mod.rs:217-228
- BackupSummary construction: see `list_remote()` at list.rs:394 and `parse_backup_summary()` at list.rs:1204
- Carried source extraction: see `collect_incremental_bases()` at list.rs:959

### Important Constraints
- Zero warnings policy: `cargo clippy` must produce no warnings
- BackupSummary has ~25 construction sites (6 production + ~19 test) that all need new field additions
- New fields use `#[serde(default)]` for backward compatibility with existing serialized manifests
- `format` param on list endpoint is for DDL compatibility only; API always returns JSON
- No runtime verification possible (no ClickHouse+S3 infra available)
