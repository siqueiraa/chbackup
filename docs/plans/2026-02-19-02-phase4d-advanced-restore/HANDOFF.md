# Handoff: Phase 4d -- Advanced Restore Modes

## Plan Location
`docs/plans/2026-02-19-02-phase4d-advanced-restore/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 10 tasks across 4 dependency groups implementing Mode A, ON CLUSTER, ZK, ATTACH TABLE, mutations |
| SESSION.md | Status tracking and phase checklists |
| acceptance.json | 10 features (F001-F009, FDOC) with 4-layer verification |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns for restore and ChClient |
| context/symbols.md | Type verification for all symbols used in plan |
| context/knowledge_graph.json | Structured JSON for symbol lookup (30+ verified symbols) |
| context/affected-modules.json | Machine-readable module status (src/restore, src/clickhouse) |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | MCP diagnostics baseline (0 errors, 0 warnings) |
| context/references.md | All 5 restore() call sites with line numbers |
| context/redundancy-analysis.md | 8 N/A, 4 EXTEND, 3 COEXIST, 0 REPLACE |
| context/git-history.md | Recent git log showing Phase 4c completion |
| context/preventive-rules-applied.md | 35 root-cause rules checked and applied |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/clickhouse/client.rs` -- 9 new ChClient methods (DROP, DETACH, ATTACH, ZK, mutation)
- `src/restore/mod.rs` -- Orchestrator: rm parameter, ATTACH TABLE mode, mutation re-apply, integration
- `src/restore/schema.rs` -- drop_tables, drop_databases, ZK conflict resolution, detect_replicated_databases
- `src/restore/topo.rs` -- engine_drop_priority, sort_tables_for_drop
- `src/restore/remap.rs` -- parse_replicated_params, resolve_zk_macros, add_on_cluster_clause, rewrite_distributed_cluster
- `src/main.rs` -- Wire rm parameter (2 call sites, remove "not yet implemented" warnings)
- `src/server/routes.rs` -- Wire rm parameter (2 call sites, add rm to RestoreRemoteRequest)
- `src/server/state.rs` -- Wire rm=false for auto_resume

### Test Files
- Unit tests embedded in each source module (`#[cfg(test)] mod tests`)
- No new test files created

### Related Documentation
- `docs/design.md` sections 5.1 (Mode A), 5.2 (ON CLUSTER), 5.3 (ATTACH TABLE), 5.7 (mutations)
- `src/restore/CLAUDE.md` -- Module documentation (Task 10 updates)
- `src/clickhouse/CLAUDE.md` -- Module documentation (Task 10 updates)

### Design Doc Section Map
| Design Section | Plan Task | Feature |
|---------------|-----------|---------|
| 5.1 Mode A | Tasks 3, 4, 8, 9 | F003, F004, F008, F009 |
| 5.2 ON CLUSTER | Tasks 2, 9 | F002, F009 |
| 5.2 DatabaseReplicated | Tasks 1, 5, 9 | F001, F005, F009 |
| 5.3 ATTACH TABLE | Tasks 1, 6, 9 | F001, F006, F009 |
| 5.7 Mutations | Tasks 1, 7, 9 | F001, F007, F009 |
| N/A ZK conflicts | Tasks 1, 2, 5 | F001, F002, F005 |
| N/A Distributed cluster | Tasks 2, 9 | F002, F009 |

### Signature Changes (Critical for Wiring)
| Function | File | Change |
|----------|------|--------|
| `restore()` | mod.rs | +1 param: `rm: bool` |
| `create_databases()` | schema.rs | +2 params: `on_cluster`, `replicated_dbs` |
| `create_tables()` | schema.rs | +4 params: `on_cluster`, `replicated_dbs`, `macros`, `dist_cluster` |
| `create_ddl_objects()` | schema.rs | +2 params: `on_cluster`, `replicated_dbs` |
| `create_functions()` | schema.rs | +1 param: `on_cluster` |

### Call Sites for restore() (from context/references.md)
1. `src/main.rs:263` -- CLI Restore command
2. `src/main.rs:385` -- CLI RestoreRemote command
3. `src/server/routes.rs:570` -- POST /api/v1/restore
4. `src/server/routes.rs:807` -- POST /api/v1/restore_remote
5. `src/server/state.rs:386` -- auto_resume
