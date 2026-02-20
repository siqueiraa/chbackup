# Handoff: Phase 4a -- Table / Database Remap

## Plan Location
`docs/plans/2026-02-18-04-phase4a-table-remap/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (5 tasks, 3 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (7 criteria: F001-F006, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns (DDL rewriting, compound command, restore signature extension) |
| context/symbols.md | Type verification (restore, create_databases, create_tables, OwnedAttachParams, etc.) |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Module status (src/restore: update, src/server: update) |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Compiler baseline (clean -- 0 errors, 0 warnings) |
| context/references.md | Reference analysis (all callers of restore(), create_tables(), etc.) |
| context/redundancy-analysis.md | New components checked (remap module is CREATE, no overlap) |
| context/git-history.md | Git context (Phase 3e complete, Phase 4a is next) |
| context/preventive-rules-applied.md | Applied rules (RC-006, RC-008, RC-015, RC-016, RC-019, RC-021) |

## Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| 1 | 22215712 | Create remap module with parsing and DDL rewriting |
| 2 | 8fff26f1 | Integrate remap into restore pipeline |
| 3 | f5120d91 | Wire CLI dispatch for --as, -m flags and restore_remote command |
| 4 | b9e497b9 | Pass remap parameters through restore and restore_remote API routes |
| 5 | 68317e9b | Update CLAUDE.md for modified modules (src/restore, src/server) |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Design Doc Sections
- **Section 6**: Table Rename / Remap (6.1 `--as`, 6.2 `-m`)
- **Section 2**: Command reference (restore_remote flags)
- **Section 5.4**: S3 object isolation (automatic via UUID paths)

### Files Being Modified
- `src/restore/remap.rs` -- NEW: DDL rewriting and table name mapping
- `src/restore/mod.rs` -- Add `pub mod remap`, extend `restore()` signature with remap params
- `src/restore/schema.rs` -- Extend `create_databases()` and `create_tables()` with remap param
- `src/main.rs` -- Wire `Command::Restore` remap params, implement `Command::RestoreRemote`
- `src/server/routes.rs` -- Add `rename_as` to RestoreRequest, add both fields to RestoreRemoteRequest
- `src/server/state.rs` -- Pass `None, None` for remap in auto_resume restore caller

### Files NOT Modified
- `src/cli.rs` -- Flags already defined (`--as` at line 121/219, `-m` at line 125/223)
- `src/manifest.rs` -- Manifest is read-only during restore
- `src/restore/attach.rs` -- OwnedAttachParams receives remapped values, no struct changes
- `src/download/mod.rs` -- download() unchanged
- `src/backup/` -- Remap is restore-only
- `src/upload/` -- Remap is restore-only

### Existing Stubs to Replace
- `main.rs:234` -- `warn!("--as flag is not yet implemented, ignoring")`
- `main.rs:237` -- `warn!("--database-mapping flag is not yet implemented, ignoring")`
- `main.rs:340` -- `Command::RestoreRemote { .. } => { info!("restore_remote: not implemented in Phase 1"); }`
- `server/routes.rs:547` -- `warn!("database_mapping is not yet implemented (Phase 4a), ignoring")`

### Config Fields Used in DDL Rewriting
- `config.clickhouse.default_replica_path` -- `/clickhouse/tables/{shard}/{database}/{table}`
- `config.clickhouse.default_replica_name` -- `{replica}`
- `config.clickhouse.restore_distributed_cluster` -- empty string (if set, rewrite Distributed cluster)

### Test Files
- `src/restore/remap.rs` -- Inline `#[cfg(test)] mod tests` with ~14 unit tests
- Tests are pure (no ClickHouse, no S3, no async)

### Related Documentation
- `src/restore/CLAUDE.md` -- Updated in Task 5
- `src/server/CLAUDE.md` -- Updated in Task 5
