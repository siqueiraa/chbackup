# Affected Modules Analysis

## Summary

- **Modules to update:** 2 (src/restore, src/server)
- **Modules with no changes:** 2 (src/clickhouse, src/download)
- **New modules to create:** 0 (remap.rs is a submodule of src/restore, not a new top-level module)
- **Git base:** 763422a7

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action |
|--------|------------------|----------|--------|
| src/restore | EXISTS | new_patterns, tree_change | UPDATE |
| src/server | EXISTS | new_patterns | UPDATE |
| src/clickhouse | EXISTS | (none) | NONE |
| src/download | EXISTS | (none) | NONE |

## Files Modified Outside Modules

| File | Reason |
|------|--------|
| `src/main.rs` | Restore and RestoreRemote CLI dispatch updated to pass remap params |

## CLAUDE.md Tasks to Generate

1. **Update:** `src/restore/CLAUDE.md` -- Add remap.rs submodule, document RemapConfig struct, DDL rewriting functions, updated restore() signature, remap flow in restore pipeline
2. **Update:** `src/server/CLAUDE.md` -- Update RestoreRemoteRequest struct documentation, add remap fields to route handler descriptions

## New Files Created by This Plan

| File | Purpose |
|------|---------|
| `src/restore/remap.rs` | DDL rewriting and table name mapping for `--as` and `-m` flags |

## Architecture Notes

The remap feature is entirely contained within the restore pipeline:
1. CLI parses `--as` and `-m` flags (already defined in `cli.rs`)
2. `main.rs` passes parsed remap params to `restore()`
3. `restore()` builds a `RemapConfig` and uses it to:
   - Map manifest table keys to destination db/table names
   - Rewrite DDL before passing to `create_databases()` and `create_tables()`
   - Pass remapped db/table names to `OwnedAttachParams`
4. The `restore_remote` command chains download + restore, passing remap params to restore step
5. No changes to backup, upload, or download modules
