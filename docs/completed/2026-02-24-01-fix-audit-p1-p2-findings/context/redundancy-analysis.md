# Redundancy Analysis

## New Public Components Proposed

This plan primarily modifies existing code (fixing bugs). The potential new public API items are:

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| (none -- no new public API) | - | N/A | All fixes modify existing functions/patterns |

## Analysis

All seven findings are fixes to existing code paths:

1. **P1 Lock bypass**: Reorder `resolve_local_shortcut`/`resolve_remote_shortcut` to run BEFORE `lock_for_command` in `main.rs:run()`. No new functions.
2. **P1 Backup name collision**: Add pre-existence check before `create_dir_all` in `backup/mod.rs`. No new functions.
3. **P2 schema+data_only**: Add `conflicts_with` attribute to clap args in `cli.rs`. No new functions.
4. **P2 create --resume**: Either remove the flag from CLI or update design doc. No new functions.
5. **P2 latest/previous sort**: Change sort key in `resolve_backup_shortcut` from name to timestamp. No new functions.
6. **P2 Doctests**: If broken, fix import path. Currently passing -- may be no-op.
7. **P3 Roadmap mismatch**: Documentation fix only. No code changes.

N/A -- no new public API introduced.
