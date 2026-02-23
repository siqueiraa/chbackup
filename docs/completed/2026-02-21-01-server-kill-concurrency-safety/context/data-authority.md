# Data Authority Analysis

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Current running operation | `RunningOp` in `AppState.current_op` | id, command, cancel_token | USE EXISTING (but single-slot is bug) |
| All running operations (parallel) | `ActionLog.entries` filtered by `ActionStatus::Running` | All entries with Running status | USE EXISTING for tracking; current_op needs to become multi-slot |
| Operation cancellation | `CancellationToken` in `RunningOp.cancel_token` | cancel(), is_cancelled(), cancelled() | USE EXISTING -- token created but never passed to operation functions |
| Lock status | `PidLock` (file-based) | path, pid, command, timestamp | USE EXISTING -- but not used in server mode |
| Config retention settings | `Config.retention` + `Config.general` | backups_to_keep_local, backups_to_keep_remote | USE EXISTING via effective_retention_local/remote() |
| Remote backups for retention | `list::list_remote()` / `list::retention_remote()` | Full backup list with manifest parsing | USE EXISTING |
| Backup name validation | None | N/A | MUST IMPLEMENT -- no validation function exists anywhere in codebase |
| Concurrent op tracking | `ActionLog` (entries ring buffer) | id, command, status per entry | USE EXISTING for audit trail; MUST IMPLEMENT multi-op current_op map |

## Analysis Notes

1. **CancellationToken wiring (Finding #1):** The token is already created and stored in RunningOp. The issue is that operation functions (backup::create, upload::upload, etc.) do not accept or check a CancellationToken. The token exists in state.rs but is never consumed by the spawned task. This is a WIRING issue, not a data authority issue. The CancellationToken type from tokio_util provides all needed APIs (cancel(), is_cancelled(), cancelled().await).

2. **Concurrent operation tracking (Finding #2):** ActionLog already tracks ALL operations including concurrent ones. The issue is that `current_op` is `Option<RunningOp>` -- a single slot. When allow_parallel=true, new ops overwrite the previous RunningOp, losing the ability to kill earlier ops. The ActionLog data IS authoritative for history, but RunningOp needs to become a map for kill support.

3. **Retention data (Finding #6):** The upload module does NOT call retention. The watch module DOES call retention after upload. The design doc (section 3.6 step 7) says retention should happen after upload. The retention functions already exist (effective_retention_local/remote, retention_local, retention_remote). This is purely a wiring gap.

4. **Config reload data (Finding #8):** Config::load() and validate() already exist and work correctly. The issue is that reload only updates WatchContext.config, not AppState.config. The restart endpoint DOES update AppState via ArcSwap -- reload should do the same for non-client config (since S3/CH credentials may not change on reload).

5. **Backup name validation (Finding #5):** No validation exists anywhere. The `sanitize_name()` function in clickhouse/client.rs is for FREEZE names (replaces special chars with underscores), NOT for path traversal prevention. A new validation function is needed.

## MUST IMPLEMENT Justification

| Component | Justification |
|-----------|---------------|
| `validate_backup_name()` | No existing function validates backup names for path safety. sanitize_name() serves a different purpose (freeze name sanitization). |
| Multi-op RunningOp map | Current single-slot Option<RunningOp> cannot track multiple concurrent operations. ActionLog tracks history but not cancellable tokens. |
| CancellationToken pass-through | Operation functions currently have no cancellation parameter. Adding token.cancelled().select() to operation loops requires modifying function signatures. |
