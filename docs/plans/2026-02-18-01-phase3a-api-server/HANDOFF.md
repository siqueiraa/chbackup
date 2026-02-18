# Handoff: Phase 3a -- API Server

## Plan Location
`docs/plans/2026-02-18-01-phase3a-api-server/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | 12 task definitions with TDD steps across 4 dependency groups |
| SESSION.md | Status tracking and phase checklists |
| acceptance.json | 12 criteria with 4-layer verification (F001-F011 + FDOC) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Discovered patterns: command delegation, error propagation, axum patterns |
| context/symbols.md | Type verification table for all existing types used by the plan |
| context/knowledge_graph.json | Structured JSON for symbol lookup (Config, ChClient, S3Client, etc.) |
| context/affected-modules.json | Machine-readable: src/server (create), src/clickhouse (update) |
| context/affected-modules.md | Human-readable module impact summary |
| context/diagnostics.md | Compiler state: CLEAN at plan time, missing deps listed |
| context/references.md | MCP references for command entry points and API config |
| context/git-history.md | Recent git log showing Phase 2d completion |
| context/redundancy-analysis.md | No redundancy issues found -- all components genuinely new |
| context/preventive-rules-applied.md | 9 applicable rules, 10 non-applicable (no Kameo actors) |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `Cargo.toml` -- Add axum, tower-http, base64, axum-server, tower, http dependencies
- `src/lib.rs` -- Add `pub mod server;` declaration
- `src/main.rs` -- Wire `Command::Server` to `server::start_server()`
- `src/list.rs` -- Add `Serialize` derive to `BackupSummary`
- `src/clickhouse/client.rs` -- Add `create_integration_tables()` and `drop_integration_tables()` methods

### New Files
- `src/server/mod.rs` -- build_router(), start_server(), AppState re-exports
- `src/server/routes.rs` -- All endpoint handler functions (~20 endpoints)
- `src/server/actions.rs` -- ActionLog ring buffer + ActionEntry types
- `src/server/auth.rs` -- Basic auth middleware
- `src/server/state.rs` -- AppState, RunningOp, operation management, auto-resume

### Test Files
- Unit tests embedded in each `src/server/*.rs` module (Rust convention)
- Compile-time verification test in `src/lib.rs` (extending existing Phase 2c pattern)

### Related Documentation
- `docs/design.md` section 9 -- Full API specification, endpoint table, integration tables DDL
- `docs/design.md` section 9.1 -- Integration tables (URL engine) DDL and usage
- `docs/roadmap.md` Phase 3a section -- Component list and definition of done

### Design Doc Endpoint Reference

| Endpoint | Method | Handler Function | Task |
|----------|--------|-----------------|------|
| `/health` | GET | `health()` | 5 |
| `/api/v1/version` | GET | `version()` | 5 |
| `/api/v1/status` | GET | `status()` | 5 |
| `/api/v1/actions` | GET | `actions()` | 5 |
| `/api/v1/list` | GET | `list_backups()` | 5 |
| `/api/v1/create` | POST | `create_backup()` | 6 |
| `/api/v1/create_remote` | POST | `create_remote_backup()` | 6 |
| `/api/v1/upload/{name}` | POST | `upload_backup()` | 6 |
| `/api/v1/download/{name}` | POST | `download_backup()` | 6 |
| `/api/v1/restore/{name}` | POST | `restore_backup()` | 6 |
| `/api/v1/restore_remote/{name}` | POST | `restore_remote_backup()` | 6 |
| `/api/v1/delete/{where}/{name}` | DELETE | `delete_backup()` | 7 |
| `/api/v1/clean/remote_broken` | POST | `clean_remote_broken()` | 7 |
| `/api/v1/clean/local_broken` | POST | `clean_local_broken()` | 7 |
| `/api/v1/kill` | POST | `kill_op()` | 7 |
| `/api/v1/clean` | POST | stub (Phase 3c) | 7 |
| `/api/v1/reload` | POST | stub (Phase 3d) | 7 |
| `/api/v1/restart` | POST | stub (Phase 3d) | 7 |
| `/api/v1/tables` | GET | stub (Phase 4f) | 7 |
| `/api/v1/watch/start` | POST | stub (Phase 3d) | 7 |
| `/api/v1/watch/stop` | POST | stub (Phase 3d) | 7 |
| `/api/v1/watch/status` | GET | stub (Phase 3d) | 7 |
| `/metrics` | GET | stub (Phase 3b) | 7 |

### Config Fields Used (all verified in knowledge_graph.json)
- `api.listen` -- Server bind address (default: "localhost:7171")
- `api.username` / `api.password` -- Basic auth credentials (empty = no auth)
- `api.secure` / `api.certificate_file` / `api.private_key_file` -- TLS config
- `api.allow_parallel` -- Operation concurrency control
- `api.complete_resumable_after_restart` -- Auto-resume toggle
- `api.create_integration_tables` -- Integration table toggle
- `api.integration_tables_host` -- DNS name override for URL engine tables
- `api.watch_is_main_process` -- Watch lifecycle flag (used by Phase 3d)
- `clickhouse.data_path` -- Local backup directory base path
