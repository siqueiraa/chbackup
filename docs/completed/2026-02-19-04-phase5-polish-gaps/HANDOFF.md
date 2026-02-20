# Handoff: Phase 5 -- Polish Gaps

## Plan Location
`docs/plans/2026-02-19-04-phase5-polish-gaps/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions for 7 polish items + CLAUDE.md update task |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 10 acceptance criteria with 4-layer verification |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | API endpoint, shadow walk, error mapping, config flag patterns |
| context/symbols.md | Type verification table for all referenced types and functions |
| context/knowledge_graph.json | Structured JSON symbol lookup with verified imports |
| context/affected-modules.json | Machine-readable affected module status (server, backup, download) |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Baseline compiler/clippy state (clean, 0 warnings) |
| context/references.md | MCP-equivalent reference analysis for all 7 items |
| context/git-history.md | Recent git log and per-file history |
| context/redundancy-analysis.md | REPLACE decisions for tables_stub and restart_stub |
| context/data-authority.md | Data authority for metadata_size (item 7) |
| context/preventive-rules-applied.md | Applied preventive rules from root-causes.md |

## Commit Log

| Commit | Task | Description |
|--------|------|-------------|
| 8a4ad4ab | 3 | feat(backup): implement --skip-projections flag |
| c32b0b0d | 4 | feat(download): implement --hardlink-exists-files dedup |
| b2d5d78f | 6 | feat(cli): add structured exit codes per design 11.6 |
| 9af8fcee | 7 | feat(server): thread metadata_size through list response |
| bb6b1707 | 5a | feat(progress): add indicatif dependency and ProgressTracker |
| 80d45475 | 2 | feat(server): implement restart endpoint with ArcSwap hot-swap |
| 4780b28e | 5b | feat(upload,download): wire progress bar into parallel pipelines |
| 6cb6f1ad | 1 | feat(server): implement GET /api/v1/tables endpoint |
| 2c5671c9 | 8 | docs: update CLAUDE.md for Phase 5 polish gaps |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/server/routes.rs` -- Items 1, 2, 3, 4, 7: API endpoints, metadata_size wiring
- `src/server/mod.rs` -- Items 1, 2: Route registration updates
- `src/backup/collect.rs` -- Item 3: Projection filter in hardlink_dir and collect_parts
- `src/backup/mod.rs` -- Item 3: Pass skip_projections through create()
- `src/download/mod.rs` -- Item 4: Hardlink dedup logic in download pipeline
- `src/main.rs` -- Items 3, 4, 6: Wire flags, structured exit codes
- `src/error.rs` -- Item 6: Exit code mapping helper
- `src/list.rs` -- Item 7: Add metadata_size to BackupSummary
- `src/progress.rs` -- Item 5: New ProgressTracker module
- `src/lib.rs` -- Item 5: Declare progress module
- `Cargo.toml` -- Item 5: Add indicatif dependency
- `src/upload/mod.rs` -- Item 5b: Wire ProgressTracker into upload

### Test Files
- Unit tests are inline in each modified file (Rust convention)
- `src/server/routes.rs` -- has `#[cfg(test)]` module at bottom
- `src/backup/collect.rs` -- will add projection filter tests
- `src/download/mod.rs` -- will add hardlink dedup tests
- `src/main.rs` -- will add exit code mapping tests (or in error.rs)
- `src/progress.rs` -- will add ProgressTracker tests

### Related Documentation
- `docs/design.md` section 3.4 -- Projection skipping specification
- `docs/design.md` section 11.4 -- Progress bar and hardlink dedup specification
- `docs/design.md` section 11.6 -- Exit code specification
- `docs/design.md` section 9 -- API endpoint specification

### Stub Locations (to be replaced)
- `src/server/routes.rs:1187` -- `restart_stub()` -> Task 2
- `src/server/routes.rs:1192` -- `tables_stub()` -> Task 1
- `src/main.rs:135-136` -- skip-projections warning -> Task 3
- `src/main.rs:198-199` -- hardlink-exists-files warning -> Task 4
- `src/main.rs:280-281` -- skip-projections warning (create_remote) -> Task 3
- `src/server/routes.rs:480-481` -- hardlink-exists-files warning (API) -> Task 4
- `src/server/routes.rs:282-284` -- hardcoded 0 for metadata_size/rbac_size/config_size -> Task 7

### Design Doc Section References
- Section 2 (Commands): Flag reference table for --skip-projections, --hardlink-exists-files
- Section 3.4 (Projection skipping): `.proj/` subdirectory filtering
- Section 9 (API): `/api/v1/tables` and `/api/v1/restart` endpoint specs
- Section 11.4 (Logging): Progress bar specification
- Section 11.6 (Exit Codes): Code 0/1/2/3/4/130/143 mapping
