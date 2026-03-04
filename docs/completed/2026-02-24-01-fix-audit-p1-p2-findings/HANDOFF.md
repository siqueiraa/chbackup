# Handoff: Fix P1/P2 Audit Findings

## Plan Location
`docs/plans/2026-02-24-01-fix-audit-p1-p2-findings/`

## Directory Contents

| File | Purpose |
|------|---------|
| PLAN.md | Task definitions and TDD steps for 6 tasks across 3 dependency groups |
| SESSION.md | Status tracking and checklists |
| acceptance.json | 4-layer verification criteria (6 features: F001-F006) |
| HANDOFF.md | This file -- resume context |
| context/patterns.md | Lock scope, backup name resolution, directory creation patterns |
| context/symbols.md | Type verification table for all modified symbols |
| context/knowledge_graph.json | Structured JSON for symbol lookup |
| context/affected-modules.json | Module status -- no CLAUDE.md updates needed |
| context/affected-modules.md | Human-readable module summary |
| context/diagnostics.md | Compiler state -- clean (0 errors, 0 warnings) |
| context/references.md | Reference analysis for lock_for_command, resolve_backup_shortcut, backup::create |
| context/redundancy-analysis.md | No new components -- all fixes modify existing code |
| context/git-history.md | Recent git log, file-specific history |
| context/preventive-rules-applied.md | Applied rules (RC-002, RC-006, RC-008, RC-019, RC-021, RC-035) |

## To Resume

1. Read SESSION.md for current status
2. Check Agent Execution Status table
3. Continue from last incomplete phase/agent
4. Use `/project-execute` when planning is complete

## Key References

### Files Being Modified
- `src/cli.rs` -- Add `conflicts_with` to restore `--schema` field (Task 1)
- `src/list.rs` -- Change sort in `resolve_backup_shortcut` to timestamp (Task 2)
- `src/main.rs` -- Move lock acquisition after shortcut resolution (Task 3)
- `src/backup/mod.rs` -- Replace `create_dir_all` with existence check + `create_dir` (Task 4)
- `docs/design.md` -- Add deferred note for create `--resume` (Task 5)

### Key Functions
- `resolve_backup_shortcut` (list.rs:312) -- Core function for latest/previous resolution
- `lock_for_command` (lock.rs:140) -- Maps command+name to lock scope
- `PidLock::acquire` (lock.rs) -- Atomic lock via O_CREAT|O_EXCL
- `backup::create` (backup/mod.rs:93) -- Entry point for backup creation
- `validate_backup_name` (server/state.rs:389) -- Path traversal validation

### Test Files
- Tests for `resolve_backup_shortcut` in `src/list.rs` (lines 2616-2811)
- Lock scope tests in `src/lock.rs` (lines 293-319)
- New tests to create: `test_restore_schema_and_data_only_conflict`, `test_resolve_backup_shortcut_sorts_by_timestamp`, `test_create_backup_dir_rejects_existing`

### Audit Findings Reference
- P1-A: Lock bypass on shortcut names (src/main.rs:128)
- P1-B: Backup name collision (src/backup/mod.rs:286)
- P2-A: Restore --schema + --data-only mutual exclusion (src/cli.rs:147-152)
- P2-B: Create --resume design doc mismatch (docs/design.md:919)
- P2-C: Latest/previous sort by name vs timestamp (src/list.rs:305)
- P2-D: Doctests -- confirmed PASSING (no fix needed)
- P3: Restart mismatch -- deferred (out of scope)
