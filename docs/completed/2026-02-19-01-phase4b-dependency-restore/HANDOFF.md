# Handoff: Phase 4b -- Dependency-Aware Restore

## Plan Location
`docs/plans/2026-02-19-01-phase4b-dependency-restore/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 7 task definitions with TDD steps for phased restore architecture |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 7 criteria with 4-layer verification |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Existing restore/backup patterns analyzed |
| context/symbols.md | Type verification table for all types used |
| context/diagnostics.md | Cargo check baseline (clean) |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Machine-readable module status (3 modules) |
| context/affected-modules.md | Human-readable module summary |
| context/redundancy-analysis.md | New components checked against existing codebase |
| context/references.md | Reference analysis for key symbols |
| context/data-authority.md | Data source authority analysis |
| context/git-history.md | Git context (Phase 4a just completed) |
| context/preventive-rules-applied.md | Applied rules from root-causes.md |

## Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| 1 | 716a7364 | Add query_table_dependencies() to ChClient |
| 2 | 6250dcbf | Populate dependencies in backup::create() |
| 3 | 4ae47472 | Create restore/topo.rs |
| 4 | 87e9bb69 | Add create_ddl_objects() to schema.rs |
| 6 | 259974a2 | Add create_functions() to schema.rs |
| 5 | 2cd7a8ea | Restructure restore() for phased architecture |
| 7 | 52e82d2d | Update CLAUDE.md for all modified modules |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Design Doc Sections
- **section 5.1** -- Phased Restore Architecture (engine priority, 4 restore phases)
- **section 5.5** -- Phase 3: DDL-Only Objects (topological sort, dependency query)
- **section 5.6** -- Phase 4: Functions, Named Collections, RBAC
- **section 7.1** -- Manifest Format (dependencies field in TableManifest)

### Files Being Modified
- `src/clickhouse/client.rs` -- New `query_table_dependencies()` method + `DependencyRow` struct
- `src/backup/mod.rs` -- Populate `dependencies` field in `TableManifest` during create()
- `src/restore/topo.rs` -- NEW: Table classification, topological sort, engine priority
- `src/restore/schema.rs` -- New `create_ddl_objects()` + `create_functions()`
- `src/restore/mod.rs` -- Restructure `restore()` for phased architecture

### Files NOT Modified (signature stability)
- `src/main.rs` -- restore() signature unchanged
- `src/manifest.rs` -- TableManifest.dependencies field already exists
- `src/server/routes.rs` -- restore() callers unaffected
- `src/cli.rs` -- No new CLI flags

### Test Files
- Unit tests in `src/clickhouse/client.rs` -- DependencyRow deserialization
- Unit tests in `src/backup/mod.rs` -- Dependency population from map
- Unit tests in `src/restore/topo.rs` -- Engine priority, classification, topo sort, cycle detection
- Existing tests in `src/restore/mod.rs` -- Must continue passing after restructuring
- Existing tests in `src/restore/schema.rs` -- Must continue passing

### Key Constraints
- ClickHouse 23.3+ provides `dependencies_database`/`dependencies_table` columns
- CH < 23.3: fallback to engine-priority sort + retry loop (empty deps)
- `restore()` signature MUST NOT change (5 callers)
- `TableRow` struct MUST NOT change (separate DependencyRow instead)
- Manifest backward compatible (empty deps = old format)

## Architecture Decision Summary

| Decision | Rationale |
|----------|-----------|
| Separate `query_table_dependencies()` vs modifying `list_tables()` | Avoids breaking TableRow struct and all callers |
| `DependencyRow` private struct | Only used by one method; no need for public API |
| Kahn's algorithm for topo sort | Standard, handles cycles gracefully |
| Retry loop in `create_ddl_objects()` | Matches Go tool fallback behavior for CH < 23.3 |
| `RestorePhases.postponed_tables` empty | Phase 4c (streaming engines) is out of scope |
| Functions restore in Phase 4 | Per design doc 5.6; simple sequential DDL execution |
