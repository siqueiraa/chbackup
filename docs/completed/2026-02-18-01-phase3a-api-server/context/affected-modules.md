# Affected Modules Analysis

## Summary

- **Modules to create:** 1 (src/server)
- **Modules to update:** 1 (src/clickhouse)
- **Files modified outside modules:** 4 (main.rs, lib.rs, list.rs, Cargo.toml)
- **Git base:** 3556364

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action |
|--------|------------------|----------|--------|
| src/server | NEW | - | CREATE |
| src/clickhouse | EXISTS | new_patterns | UPDATE |

## Files Modified Outside Module Directories

| File | Reason |
|------|--------|
| src/main.rs | Wire `Command::Server` to `server::start_server()` instead of stub |
| src/lib.rs | Add `pub mod server` declaration |
| src/list.rs | Add `#[derive(Serialize)]` to `BackupSummary` for JSON API responses |
| Cargo.toml | Add dependencies: axum, tower-http, tokio-util (CancellationToken feature), base64, axum-server |

## CLAUDE.md Tasks to Generate

1. **Create:** `src/server/CLAUDE.md` -- New module documenting API server architecture, AppState, route handlers, ActionLog, auth middleware, operation lifecycle
2. **Update:** `src/clickhouse/CLAUDE.md` -- Add documentation for integration table DDL methods (`create_integration_tables`, `drop_integration_tables`)

## Architecture Impact

### New Module: src/server/

This is the primary deliverable of Phase 3a. It introduces:
- HTTP server via axum bound to `api.listen` (default: localhost:7171)
- Shared application state (AppState) with config, clients, action log
- Route handlers for all API endpoints from design doc section 9
- Action log ring buffer for operation tracking
- Basic auth middleware (optional, when username/password configured)
- TLS support (optional, when secure=true)
- Operation serialization (when allow_parallel=false)
- CancellationToken support for POST /kill
- Auto-resume on startup (scan for state files)
- Integration table lifecycle (create on start, drop on stop)

### Modified: src/clickhouse/

Small addition: two new methods on ChClient for creating and dropping the URL engine integration tables (`system.backup_list`, `system.backup_actions`). These call `execute_ddl()` with the SQL from design doc section 9.1.
