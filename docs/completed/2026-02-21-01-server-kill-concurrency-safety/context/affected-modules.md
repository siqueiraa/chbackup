# Affected Modules Analysis

## Summary

- **Modules to update:** 7
- **Modules to create:** 0
- **Git base:** HEAD (master branch)

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action | Files Modified |
|--------|------------------|----------|--------|----------------|
| src/server | EXISTS | new_patterns | UPDATE | routes.rs, state.rs, mod.rs |
| src/lock.rs | N/A (single file) | new_patterns | UPDATE | lock.rs |
| src/backup | EXISTS | - | UPDATE | mod.rs |
| src/upload | EXISTS | new_patterns | UPDATE | mod.rs |
| src/watch | EXISTS | - | NO_CHANGE | (none -- existing retention logic is reused, not modified) |
| src/main.rs | N/A (single file) | - | UPDATE | main.rs |
| src/list.rs | N/A (single file) | - | UPDATE | list.rs |
| test | N/A (test dir) | new_patterns | UPDATE | run_tests.sh |

## CLAUDE.md Update Tasks

1. **Update:** src/server/CLAUDE.md -- Document multi-op RunningOp map, cancellation token wiring, backup name validation, DRY orchestration pattern, reload semantics
2. **Update:** src/upload/CLAUDE.md -- Document auto-retention after upload
3. **Update:** src/backup/CLAUDE.md -- Document validate_backup_name() usage

## Architecture Assumptions (VALIDATED)

### Component Ownership

- **AppState**: Created by `AppState::new()` in server/state.rs, stored by axum State extractor (shared via Clone), accessed by all route handlers via `State<AppState>`
- **RunningOp**: Created by `try_start_op()`, stored in `AppState.current_op`, accessed by `kill_current()`, `finish_op()`, `fail_op()`
- **ActionLog**: Created in `AppState::new()`, stored in `AppState.action_log`, accessed by `try_start_op()`, `finish_op()`, `fail_op()`, `kill_current()`, `get_actions()`, `post_actions()`
- **PidLock**: Created by `PidLock::acquire()` in main.rs, stored as local variable `_lock_guard`, NOT used by server mode
- **CancellationToken**: Created in `try_start_op()`, clone stored in `RunningOp.cancel_token`, DISCARDED by all route handlers (`_token`)
- **Config (server)**: Created from file, stored in `AppState.config` via ArcSwap, accessed by all handlers via `.load()`
- **Config (watch)**: Stored in `WatchContext.config` as `Arc<Config>`, updated by `apply_config_reload()`, NOT synced with AppState.config
- **Retention functions**: Defined in list.rs, called by watch/mod.rs during watch cycle, NOT called by upload/mod.rs or server/routes.rs upload handler

### What This Plan CANNOT Do

- **Cannot add cooperative cancellation to long-running ClickHouse queries**: `ChClient` uses the `clickhouse` crate which does not expose query cancellation. Cancellation can only abort between steps (between FREEZE and shadow walk, between part uploads, etc.), not mid-query.
- **Cannot make PidLock fully race-free without OS-level file locking**: The TOCTOU fix can use `O_CREAT|O_EXCL` or `flock()`, but these have different semantics on NFS vs local filesystems. The plan should use the most portable approach.
- **Cannot make server-mode PID locks consistent with CLI without architectural changes**: The server runs a single long-lived process handling multiple operations. PID locks are per-operation in CLI. The server would need to acquire/release per-operation PID locks within the spawned task scope. This is possible but the lock file scope must be within the spawned task (not the handler).
- **Cannot retroactively cancel already-running operations after `kill`**: Once `kill_current()` fires, the operation task must check `token.is_cancelled()` at checkpoints. Existing operation functions have no such checkpoints. Full cooperative cancellation requires modifying backup::create, upload::upload, download::download, and restore::restore to accept and check tokens.
