# Redundancy Analysis

## New Public Components Proposed

Phase 3a introduces a new `src/server/` module. All components below are new with no existing equivalents.

| Proposed | Existing Match (fully qualified) | Decision | Task/Acceptance | Justification |
|----------|----------------------------------|----------|-----------------|---------------|
| `server::AppState` | None | N/A | - | New shared state struct for axum handlers. No existing equivalent. |
| `server::ActionLog` | None | N/A | - | New in-memory ring buffer for operation history. CLI has no equivalent. |
| `server::ActionEntry` | None | N/A | - | New struct for individual action records. No existing equivalent. |
| `server::build_router()` | None | N/A | - | New function to construct axum Router. No existing equivalent. |
| `server::start_server()` | None | N/A | - | New function to bind and run the HTTP server. No existing equivalent. |
| `server::auth_middleware()` | None | N/A | - | New Basic auth middleware. No existing equivalent. |

## Potential Overlap Analysis

### list::Location vs cli::Location
Both `list::Location` and `cli::Location` are `{Local, Remote}` enums. `main.rs` already has `map_cli_location()` to convert between them. The server will use `list::Location` directly (no need for cli::Location since API params come from JSON, not clap).

**Decision:** REUSE `list::Location`. No new Location enum needed.

### BackupSummary serialization
`BackupSummary` in `list.rs` is `#[derive(Debug, Clone)]` but NOT `Serialize`. The API needs JSON serialization of backup summaries for `GET /api/v1/list`.

**Decision:** EXTEND -- Add `#[derive(Serialize)]` to `BackupSummary`. This is a non-breaking additive change.

### Command dispatch: main.rs vs server routes
`main.rs` has the command dispatch logic (match arms calling backup::create, upload::upload, etc.). Server route handlers will call the same functions but with parameters from JSON bodies instead of CLI args.

**Decision:** REUSE existing command functions. Route handlers are thin wrappers that extract JSON params and call the same functions. No duplication of command logic.

### Lock management
Server runs as a long-lived process. CLI's per-process PidLock pattern still works for backup-scoped locks, but the server also needs operation serialization (Mutex/Semaphore) for the `allow_parallel=false` case.

**Decision:** REUSE PidLock for per-backup file locks. ADD new operation Mutex in AppState for request serialization. These serve different purposes (file lock = cross-process, Mutex = intra-process).

## Summary

No REPLACE or COEXIST decisions needed. All new components are genuinely new with no existing equivalents. One EXTEND decision (adding Serialize to BackupSummary). All existing command functions are REUSED without duplication.
