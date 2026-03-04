# Affected Modules Analysis

## Summary

- **Files to modify:** 5
- **Module CLAUDE.md updates needed:** 0
- **New modules to create:** 0

## Files Being Modified

| File | Finding | Change Type |
|------|---------|-------------|
| src/main.rs | P1 lock bypass, P2 create --resume | Reorder shortcut resolution before lock; update/remove resume comment |
| src/cli.rs | P2 schema+data_only, P2 create --resume | Add `conflicts_with`; possibly remove resume from Create |
| src/list.rs | P2 latest/previous sort | Change sort in `resolve_backup_shortcut` to use timestamp |
| src/backup/mod.rs | P1 name collision | Add pre-existence check before `create_dir_all` |
| src/lock.rs | (tests only) | Add test for shortcut locking behavior |

## Module CLAUDE.md Status

| Module | CLAUDE.md Status | Triggers | Action |
|--------|-----------------|----------|--------|
| src/backup | EXISTS | none | NO UPDATE (bug fix, no new patterns) |
| src/restore | EXISTS | none | NO UPDATE (no restore code changes) |
| src/server | EXISTS | none | NO UPDATE (no server code changes) |

## Notes

- `src/path_encoding.rs` was listed as a finding but doctests currently pass. May be a no-op.
- `docs/design.md` and `docs/roadmap.md` need documentation-only fixes for P2/P3 findings.
- No module-level structural changes -- all fixes are to existing functions.
