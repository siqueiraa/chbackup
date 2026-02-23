# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (35 rules total)
- `.claude/skills/self-healing/references/planning-rules.md` (14 planning-scoped rules)

## Rules Applicable to This Plan

### RC-006: Plan code snippets use unverified APIs
**Applicability:** HIGH -- plan will contain code snippets for CancellationToken wiring, PidLock changes, validation functions, and DRY refactoring.
**Action:** All code snippets in PLAN.md must be verified against actual crate APIs using grep/LSP before inclusion.

### RC-008: TDD task sequencing violation
**Applicability:** MEDIUM -- multiple interdependent tasks (e.g., kill token wiring depends on current_op changes, DRY refactor depends on operation trait).
**Action:** Ensure tasks that introduce new types/fields precede tasks that consume them.

### RC-011: State machine flags missing exit path transitions
**Applicability:** HIGH -- CancellationToken lifecycle is a state machine concern. The token must be cancelled on all exit paths (success, failure, kill). Current issue: token is created but not passed to spawned tasks.
**Action:** Verify cancellation token is wired into all operation exit paths.

### RC-017: State field declaration missing
**Applicability:** MEDIUM -- new fields on AppState (e.g., concurrent_ops map) must be declared and initialized.
**Action:** Verify any new AppState fields have explicit initialization in AppState::new().

### RC-019: Existing implementation pattern not followed
**Applicability:** HIGH -- DRY refactoring must follow existing patterns (try_start_op lifecycle, metrics instrumentation, etc.)
**Action:** When extracting common patterns, verify the refactored code matches all existing handler patterns exactly.

### RC-021: Struct/field file location assumed without verification
**Applicability:** HIGH -- must verify actual locations of AppState, RunningOp, PidLock, and other types.
**Action:** All struct modifications verified with grep for actual file location.
**Verified:**
- `AppState` -> `src/server/state.rs:65`
- `RunningOp` -> `src/server/state.rs:88`
- `PidLock` -> `src/lock.rs:27`
- `ActionLog` -> `src/server/actions.rs:50`
- `ActionStatus` -> `src/server/actions.rs:14`
- `ChBackupError` -> `src/error.rs:5`
- `Config` -> `src/config.rs` (RetentionConfig at line ~392)
- `WatchContext` -> `src/watch/mod.rs`

### RC-032: Adding tracking/calculation without verifying data source authority
**Applicability:** LOW -- this plan is about correctness fixes, not adding new tracking. But the parallel op tracking (finding #2) needs to verify what data already exists in ActionLog vs what RunningOp tracks.
**Action:** Verify ActionLog already tracks concurrent operations (it does -- `running()` only returns first, but `entries()` shows all). Data authority analysis done in data-authority.md.

## Rules NOT Applicable

| Rule | Reason |
|------|--------|
| RC-001 | No Kameo actors in this project |
| RC-002 | No financial data types |
| RC-004 | No message handlers |
| RC-005 | No division operations in scope |
| RC-010 | No adapter stubs |
| RC-012 | No E2E callbacks |
| RC-013 | No std::sync::Mutex in async test code (plan uses tokio::sync::Mutex) |
| RC-014 | No connection polling loops |
| RC-015 | No cross-task data flow mismatches anticipated |
| RC-016 | No new struct definitions for consumer tasks |
| RC-020 | No Kameo message types |
