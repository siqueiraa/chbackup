# Handoff: Server API Kill, Concurrency, and Safety Fixes

## Plan Location
`docs/plans/2026-02-21-01-server-kill-concurrency-safety/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps (10 tasks, 4 dependency groups) |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (10 criteria: F001-F009 + FDOC) |
| HANDOFF.md | This file - resume context |
| context/patterns.md | Operation lifecycle patterns, boilerplate analysis |
| context/symbols.md | Type verification for AppState, RunningOp, PidLock, CancellationToken |
| context/knowledge_graph.json | Structured JSON for symbol lookup (18 verified symbols) |
| context/affected-modules.json | Machine-readable module status (7 modules affected) |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Compiler state (zero errors, zero warnings) |
| context/references.md | Reference analysis for key symbols (try_start_op: 15 call sites, current_op: 8 references) |
| context/redundancy-analysis.md | New vs existing component analysis (4 decisions) |
| context/git-history.md | Recent git log and file-specific history |
| context/preventive-rules-applied.md | 7 applicable root-cause rules |
| context/data-authority.md | Data source verification |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/server/state.rs` - AppState struct, RunningOp, try_start_op/finish_op/fail_op/kill, run_operation helper, validate_backup_name
- `src/server/routes.rs` - All 11 operation handlers (DRY refactored), kill_op, reload, status
- `src/server/mod.rs` - Possible import changes
- `src/lock.rs` - PidLock::acquire() TOCTOU fix (create_new atomicity)
- `src/list.rs` - apply_retention_after_upload() helper
- `src/main.rs` - CLI upload auto-retention call, create --resume comment
- `test/run_tests.sh` - T4-T10 integration tests

### Test Files
- `src/server/state.rs` (inline #[cfg(test)] mod tests) - validate_backup_name, running_ops, cancellation
- `src/server/routes.rs` (inline #[cfg(test)] mod tests) - reload, DRY helper
- `src/lock.rs` (inline #[cfg(test)] mod tests) - atomic lock acquisition
- `test/run_tests.sh` - Integration tests T4-T10

### Related Documentation
- `docs/design.md` - Sections 2 (CLI), 3.6 (upload pipeline), 9 (API), 10.8 (hot-reload)
- `src/server/CLAUDE.md` - Module documentation (to be updated in Task 10)

## Critical Findings from Analysis

1. **CancellationToken discarded**: All 14 call sites bind to `_token`. Token is created but never reaches spawned tasks. Kill endpoint calls `cancel()` but nothing checks `is_cancelled()`.
2. **Single-slot current_op**: When `allow_parallel=true`, `try_start_op` overwrites previous RunningOp at `state.rs:167`. Only latest op is killable/trackable.
3. **PID lock TOCTOU**: `path.exists()` at `lock.rs:38` then `fs::write()` at `lock.rs:72` is non-atomic. Fix uses `OpenOptions::create_new(true)`.
4. **Path traversal**: Backup names go directly into `PathBuf::join()` without validation. `../` in name escapes backup directory.
5. **Upload missing retention**: Design doc 3.6 step 7 says retention after upload; only watch loop does it.
6. **Reload is a no-op**: Without watch, reload returns "reloaded" without changing anything.

## Commit History

| Task | Commit | Description |
|------|--------|-------------|
| 1 | f13aebd0 | feat(server): add backup name path traversal validation |
| 2 | ccbc36a1 | fix(lock): eliminate TOCTOU race in PidLock::acquire via O_CREAT\|O_EXCL |
| 3 | 38c21233 | feat(server): replace single-slot current_op with running_ops HashMap |
| 4 | 989639b0 | feat(server): wire CancellationToken into all 11 route handlers via tokio::select! |
| 5 | e24d33e2 | refactor(server): extract DRY run_operation helper for all route handlers |
| 6 | c75af0f8 | fix(server): make reload update AppState config+clients and watch loop clients |
| 7 | 59f08788 | feat(list): add auto-retention after upload for CLI and API handlers |
| 8 | 3973d090 | docs(main): clarify create --resume as intentionally deferred design decision |
| 9 | b4e8b6b7 | test: add integration tests T4-T10 |
| 10 | a5774991 | docs(server): update CLAUDE.md for running_ops, run_operation, kill, validation, reload changes |

## Dependency Order

Groups A and B can run in parallel. Group C depends on Group B. Group D depends on all.

```
Group A: Task 1 -> Task 2
Group B: Task 3 -> Task 4 -> Task 5
Group C: Task 6 -> Task 7           (after Group B)
Group D: Task 8 -> Task 9 -> Task 10 (after all)
```
