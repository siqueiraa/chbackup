# Redundancy Analysis

## Proposed New Components

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `validate_backup_name(name: &str) -> Result<()>` | `sanitize_name()` in clickhouse/client.rs:1293 | COEXIST | - | Different purpose: sanitize_name replaces special chars for FREEZE names; validate_backup_name rejects path traversal (../, /, NUL). Cleanup deadline: N/A -- both are needed permanently. |
| `AppState.running_ops: HashMap<u64, RunningOp>` (replacing current_op) | `AppState.current_op: Arc<Mutex<Option<RunningOp>>>` | REPLACE | Task for replacement + Task for tests | current_op is single-slot; running_ops supports parallel kill. Old field removed in same task. |
| DRY orchestration helper (e.g., `run_operation()` or trait) | 11 copy-pasted handler bodies in routes.rs | REPLACE | DRY refactor task | Reduces ~500 lines of duplicated try_start_op/spawn/metrics/finish_op boilerplate. Old handlers replaced in same task. |
| `apply_retention_after_upload()` helper | `watch::mod.rs:490-527` (retention in watch loop) | REUSE | - | Watch loop already has the exact retention logic. Extract and reuse in upload path. |

## Detailed Analysis

### validate_backup_name vs sanitize_name
- `sanitize_name(name: &str) -> String` (clickhouse/client.rs:1293): Replaces non-alphanumeric chars (except underscore) with underscore. Purpose: generate safe ClickHouse FREEZE names. Does NOT reject input, always succeeds.
- `validate_backup_name(name: &str) -> Result<()>` (proposed): Rejects names containing `..`, `/`, `\`, NUL bytes, or empty strings. Purpose: prevent path traversal in filesystem operations.
- These serve fundamentally different purposes and MUST coexist.

### running_ops map vs current_op
- `current_op: Arc<Mutex<Option<RunningOp>>>`: Single slot, only tracks most recent operation.
- `running_ops: Arc<Mutex<HashMap<u64, RunningOp>>>`: Maps operation ID to RunningOp, supports killing any operation by ID.
- The replacement MUST maintain the existing `kill_current()` API semantics (or extend to `kill_op(id)`) while removing the single-slot limitation.
- REPLACE decision includes: task to migrate current_op to running_ops, task to update all tests that reference current_op, acceptance criteria verifying old single-slot code is absent.

### DRY orchestration
- All 11 operation handlers in routes.rs follow the identical pattern (Pattern 1 from patterns.md).
- Extracting a helper function or macro will reduce maintenance burden and ensure future handlers get metrics/kill/lifecycle for free.
- The refactored code must preserve ALL existing behavior including per-operation metrics labels, manifest cache invalidation, and specific error handling.

### Retention after upload
- Watch loop (watch/mod.rs:490-527) calls `retention_local()` then `retention_remote()` after successful upload.
- The upload API handler and CLI upload command do NOT call retention.
- Design doc section 3.6 step 7: "Apply retention: delete oldest remote backups exceeding backups_to_keep_remote"
- Solution: extract retention into a reusable helper and call from upload handler (guarded by config).
