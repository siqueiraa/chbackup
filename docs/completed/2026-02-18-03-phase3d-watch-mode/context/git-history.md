# Git Context -- Phase 3d Watch Mode

## Current Branch

**Branch:** `master`
**Divergence from main:** On main branch (or no divergence)

## Recent Repository History (30 commits)

```
9e889020 docs: Archive completed plan 2026-02-18-03-phase3c-retention-gc
a9ed0f10 docs: Mark plan as COMPLETED
487128b9 style: apply cargo fmt formatting
cbaea0ab docs: update CLAUDE.md for Phase 3c retention/GC/clean changes
e8e2c1fb feat: wire clean command CLI dispatch and replace clean_stub API handler
4b9cf112 feat(list): add GC-safe deletion and remote retention
83433042 feat(list): add GC key collection for safe remote backup deletion
ad1aed15 feat(list): add config resolution helpers and local retention
df213012 chore: remove debug markers for Phase 3b metrics
ea472341 docs(server): update CLAUDE.md with metrics module documentation
52b933a5 feat(server): instrument operation handlers with prometheus metrics
09f1603c feat(server): replace metrics_stub with real /metrics handler
9c364c1f feat(server): add metrics field to AppState with conditional creation
2e08025c feat(server): add Metrics struct with 14 prometheus metric definitions
b0fa80ef feat(deps): add prometheus 0.13 dependency for Phase 3b metrics
3d6913e3 feat: add API server module (Phase 3a)
bc7fcd42 style: apply cargo fmt to main.rs, update Cargo.lock
1de06648 docs: Mark plan as COMPLETED
816cc979 style: apply cargo fmt formatting to server module files
ef478976 docs(server): add CLAUDE.md for server module, update clickhouse CLAUDE.md
6229910d feat(server): wire Command::Server to start_server in main.rs
c85176f0 feat(server): add auto-resume for interrupted operations on restart
37666738 feat(clickhouse): add integration table DDL methods for API server
e76f3a9e feat(server): add router assembly and server startup with TLS support
df76cd9f feat(server): add delete, clean, kill, and stub endpoints
9c9abdfa feat(server): add backup operation endpoints for create, upload, download, restore, create_remote, restore_remote
3bcfa445 feat(server): add read-only route handlers for health, version, status, actions, and list
64565293 feat(server): add Basic auth middleware for API endpoints
9080fed3 feat(server): add AppState and operation management with concurrency control
c5264fce feat(server): add ActionLog ring buffer and ActionEntry types
```

## File-Specific History (key files for Phase 3d)

### src/server/ + src/list.rs + src/config.rs + src/main.rs + src/cli.rs

```
487128b9 style: apply cargo fmt formatting
cbaea0ab docs: update CLAUDE.md for Phase 3c retention/GC/clean changes
e8e2c1fb feat: wire clean command CLI dispatch and replace clean_stub API handler
4b9cf112 feat(list): add GC-safe deletion and remote retention
83433042 feat(list): add GC key collection for safe remote backup deletion
ad1aed15 feat(list): add config resolution helpers and local retention
df213012 chore: remove debug markers for Phase 3b metrics
ea472341 docs(server): update CLAUDE.md with metrics module documentation
52b933a5 feat(server): instrument operation handlers with prometheus metrics
09f1603c feat(server): replace metrics_stub with real /metrics handler
```

## Phase Progression Context

The project has completed Phases 0 through 3c:

1. **Phase 0** (skeleton) -- CLI, config, clients
2. **Phase 1** (MVP) -- Single-table backup/restore
3. **Phase 2a** (parallelism) -- Concurrent operations, multipart upload
4. **Phase 2b** (incremental) -- diff-from, create_remote
5. **Phase 2c** (S3 object disk) -- Metadata parsing, CopyObject
6. **Phase 2d** (resume & reliability) -- State files, CRC64 verify
7. **Phase 3a** (API server) -- axum HTTP server, all endpoints
8. **Phase 3b** (Prometheus metrics) -- 14 metric families
9. **Phase 3c** (retention/GC/clean) -- Local/remote retention, GC, clean_shadow

**Phase 3d** (watch mode) is the next milestone. It depends on:
- API server (3a) -- for `server --watch` mode and `/api/v1/watch/*` endpoints
- Prometheus metrics (3b) -- for watch state/timestamp/error gauges
- Retention/GC (3c) -- for post-upload cleanup in the watch loop
- Existing `create` + `upload` pipelines -- the core of each watch cycle

## Commit Style

The project uses conventional commits:
- `feat:` / `feat(module):` for features
- `fix:` / `fix(module):` for bug fixes
- `docs:` for documentation
- `style:` for formatting
- `chore:` for maintenance
- `refactor:` for refactoring
- `test:` for tests
