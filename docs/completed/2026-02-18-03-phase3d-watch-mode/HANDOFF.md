# Handoff: Phase 3d -- Watch Mode

## Plan Location
`docs/plans/2026-02-18-03-phase3d-watch-mode/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps for 10 tasks across 4 dependency groups |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 11 acceptance criteria with 4-layer verification |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Pattern analysis: operation lifecycle, duration parsing, SIGHUP, config reload |
| context/symbols.md | Type verification table for all watch-related types and API signatures |
| context/knowledge_graph.json | Verified symbol lookup with exact import paths and locations |
| context/diagnostics.md | Compiler state (clean), existing stubs inventory, missing features inventory |
| context/references.md | Symbol references: callers of start_server, existing watch CLI/config code |
| context/git-history.md | Recent 30 commits, phase progression context |
| context/affected-modules.json | Machine-readable: 1 module to create, 2 to update, 4 files to modify |
| context/affected-modules.md | Human-readable affected modules summary |
| context/redundancy-analysis.md | All new components verified: no duplication, stubs being replaced |
| context/data-authority.md | Data sources: 9 existing, 2 to implement (get_macros, SIGHUP) |
| context/preventive-rules-applied.md | 8 rules applied, 6 not applicable |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Design Doc Sections
- **Section 10**: Watch Mode (complete section 10.1-10.9)
- **Section 10.3**: Name template configuration and macro resolution
- **Section 10.4**: State machine diagram (Resume -> Decide -> Create -> Upload -> Delete -> Retention -> Sleep)
- **Section 10.5**: Resume on restart algorithm
- **Section 10.6**: Error handling and full-backup fallback
- **Section 10.8**: Config hot-reload via SIGHUP

### Files Being Created
- `src/watch/mod.rs` -- New module: state machine, name template, resume logic
- `src/watch/CLAUDE.md` -- New module documentation

### Files Being Modified
- `src/config.rs` -- Make `parse_duration_secs` public, add `tables` field to WatchConfig
- `src/clickhouse/client.rs` -- Add `get_macros()` method and `MacroRow` struct
- `src/server/mod.rs` -- Watch loop spawn, SIGHUP handler, `start_server()` signature change
- `src/server/state.rs` -- Watch fields on AppState, WatchStatus struct
- `src/server/routes.rs` -- Replace 4 stub endpoints with real implementations
- `src/lib.rs` -- Add `pub mod watch;`
- `src/main.rs` -- Wire standalone watch command, pass watch flag to server

### Existing Stubs Being Replaced
- `routes::reload_stub` -> `routes::reload` (routes.rs:1096)
- `routes::watch_start_stub` -> `routes::watch_start` (routes.rs:1111)
- `routes::watch_stop_stub` -> `routes::watch_stop` (routes.rs:1116)
- `routes::watch_status_stub` -> `routes::watch_status` (routes.rs:1121)

### Existing Infrastructure Being Reused
- `backup::create()` -- src/backup/mod.rs:64
- `upload::upload()` -- src/upload/mod.rs:165
- `list::list_remote()` -- src/list.rs:128
- `list::delete_local()` -- src/list.rs:220 (sync fn -- needs `spawn_blocking`)
- `list::retention_local()` -- src/list.rs:411 (sync fn -- needs `spawn_blocking`)
- `list::retention_remote()` -- src/list.rs:629 (async)
- `list::effective_retention_local()` -- src/list.rs:380
- `list::effective_retention_remote()` -- src/list.rs:393
- `Metrics` watch gauges -- src/server/metrics.rs (already registered, just need updating)

### Implementation Notes
- **Sync functions in async context**: `delete_local()` and `retention_local()` are sync (`fn`), must be called via `tokio::task::spawn_blocking` in the watch loop (matches server/routes.rs pattern)
- **Config reload validation**: After `Config::load()`, call `config.validate()` before applying; on failure, log warning and keep current config (design 10.8 step 3b)
- **Template prefix filtering**: `resume_state()` filters remote backups by the static prefix of the name template to avoid picking up unrelated backups (design 10.5)

### Related Documentation
- `src/server/CLAUDE.md` -- Server module patterns (operation lifecycle, route handlers)
- `src/clickhouse/CLAUDE.md` -- ClickHouse client patterns (Row types, query methods)
- `docs/design.md` -- Full technical spec
- `docs/roadmap.md` -- Phase progression
