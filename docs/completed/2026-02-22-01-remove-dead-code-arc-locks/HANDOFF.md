# Handoff: Remove Dead Code, Unused Fields, and Unnecessary Public APIs

## Plan Location
`docs/plans/2026-02-22-01-remove-dead-code-arc-locks/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (4 tasks, 2 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (4 features: F001, F002, F003, FDOC) |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Dead code patterns discovered (5 pattern categories) |
| context/symbols.md | Type verification for all 8 dead items + Arc/Mutex analysis |
| context/diagnostics.md | cargo check/clippy baseline, LSP findReferences results |
| context/references.md | Detailed LSP reference counts and grep evidence for each item |
| context/git-history.md | Commit history for affected files |
| context/knowledge_graph.json | Structured JSON for all verified symbols |
| context/affected-modules.json | Module status (3 to update, 1 unchanged) |
| context/affected-modules.md | Human-readable module summary |
| context/redundancy-analysis.md | N/A (removing code, not adding) |
| context/data-authority.md | N/A (removing code, not adding tracking) |
| context/preventive-rules-applied.md | Applied preventive rules |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/clickhouse/client.rs` -- Remove `debug` field + `#[allow(dead_code)]` + `inner()` getter
- `src/storage/s3.rs` -- Remove `inner()`, `concurrency()`, `object_disk_path()` getters + `concurrency`/`object_disk_path` fields + update test helper
- `src/restore/attach.rs` -- Remove `attach_parts()` function + `#[allow(dead_code)]`

### Documentation Files
- `src/clickhouse/CLAUDE.md` -- Remove inner() and debug from docs
- `src/storage/CLAUDE.md` -- Remove inner(), concurrency(), object_disk_path() from docs
- `src/restore/CLAUDE.md` -- Remove attach_parts() from docs

### What NOT to Touch
- `src/server/state.rs` -- All Arc/Mutex/ArcSwap is required
- `src/server/metrics.rs` -- Unused counters kept by convention
- `src/error.rs` -- Error variant taxonomy preserved
- `src/progress.rs` -- Test-only helpers preserved
- `src/server/actions.rs` -- Test-only helpers preserved
- `src/config.rs` -- Config fields preserved for forward compatibility

## Commit Log

| Task | Commit | Description |
|------|--------|-------------|
| 1 | 5b912ba4 | refactor(clickhouse): remove dead debug field and unused inner() getter from ChClient |
| 2 | 9edc6e3c | refactor(storage): remove unused inner(), concurrency(), object_disk_path() getters and dead fields from S3Client |
| 3 | c6152ac8 | refactor(restore): remove dead attach_parts() function superseded by attach_parts_owned() |
| 4 | fbc227e4 | docs: update CLAUDE.md files to reflect dead code removal from ChClient, S3Client, and attach.rs |

## Plan Characteristics

- **Type:** Pure refactoring (deletion only)
- **Risk:** GREEN across all categories
- **Runtime verification:** Not applicable (no behavioral changes)
- **Tasks:** 4 total (3 independent code tasks + 1 documentation task)
- **Dead items removed:** 8 (2 fields, 5 methods, 1 function)
- **#[allow(dead_code)] eliminated:** 2 annotations
