# Handoff: Phase 4c -- Streaming Engine Postponement

## Plan Location
`docs/plans/2026-02-19-02-phase4c-streaming-postpone/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (4 tasks, 2 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (F001, F002, F003, FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Engine classification patterns from backup and restore modules |
| context/symbols.md | Type verification for RestorePhases, TableManifest, classify_restore_tables |
| context/knowledge_graph.json | Structured JSON for verified symbol lookup |
| context/affected-modules.json | Module status: src/restore (update) |
| context/affected-modules.md | Human-readable module summary |
| context/redundancy-analysis.md | is_streaming_engine vs is_engine_excluded analysis |
| context/diagnostics.md | Compiler state: zero errors, zero warnings |
| context/references.md | Reference analysis for RestorePhases, classify_restore_tables, etc. |
| context/git-history.md | Git context for topo.rs, mod.rs, schema.rs |
| context/preventive-rules-applied.md | Applied rules: RC-006, RC-008, RC-015, RC-018, RC-019, RC-021 |
| context/data-authority.md | Data authority verification (DDL ordering logic, no tracking) |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/restore/topo.rs` -- Add `is_streaming_engine()`, `is_refreshable_mv()`; modify `classify_restore_tables()` to populate `postponed_tables`
- `src/restore/mod.rs` -- Add Phase 2b execution block between data attachment and Phase 3 DDL
- `src/restore/CLAUDE.md` -- Update module documentation for Phase 2b

### Files NOT Modified (read-only reference)
- `src/restore/schema.rs` -- `create_tables()` reused as-is for postponed tables
- `src/manifest.rs` -- `TableManifest` struct (engine, ddl, metadata_only fields)
- `src/backup/mod.rs` -- `is_metadata_only_engine()` pattern reference

### Test Files
- `src/restore/topo.rs` (inline `#[cfg(test)]` module) -- Unit tests for detection functions and classification

### Related Documentation
- `docs/design.md` section 5.1 -- Phased restore architecture, Phase 2b specification
- `docs/roadmap.md` Phase 4c -- Streaming Engine Postponement deliverables

## Design Doc Quick Reference

Design doc section 5.1 defines Phase 2b:
```
Phase 2b: Postponed tables -- Streaming engines (Kafka, NATS, RabbitMQ, S3Queue)
          and refreshable MVs. Created AFTER all data is attached (#1235).
          These engines start consuming immediately on CREATE, so they must
          be activated only after the target tables have their data restored.
```

## Key Decisions

1. **Streaming engines are hardcoded, not config-driven**: Unlike `skip_table_engines` (user config), `is_streaming_engine()` is a fixed set of four engine names. This is a safety measure, not a preference.

2. **REFRESH detection is DDL-based**: No ClickHouse system table query needed. The REFRESH keyword is detected in the DDL string with case-insensitive matching.

3. **`create_tables()` is reused for Phase 2b**: No new schema creation function needed. The existing function handles IF NOT EXISTS, remap, and data_only mode.

4. **Schema-only mode ordering**: In schema-only mode, postponed tables are created AFTER Phase 3 DDL-only objects (since DDL-only objects like regular MVs may be targets that streaming engines write to).

## Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| 1 | eabb2009 | feat(restore): add streaming engine and refreshable MV detection functions |
| 2 | ceeb65f3 | feat(restore): populate postponed_tables in classify_restore_tables |
| 3 | ec926658 | feat(restore): add Phase 2b execution block for postponed tables |
| 4 | 62b45a1b | docs(restore): update CLAUDE.md for Phase 4c streaming engine postponement |
