# Affected Modules Analysis

## Summary

- **Files to modify:** 4
- **CLAUDE.md to update:** 1 (src/server/)
- **No change needed:** 3 (config.rs, lock.rs, error.rs)
- **Git base:** df21301

## Files Being Modified

| File/Module | CLAUDE.md Status | Triggers | Action |
|-------------|------------------|----------|--------|
| src/list.rs | N/A (single file) | new_functions | MODIFY -- add 7 new public functions |
| src/server/routes.rs | EXISTS (parent) | stub_replacement | MODIFY -- replace clean_stub with real handler |
| src/server/mod.rs | EXISTS (parent) | route_update | MODIFY -- update route wiring if needed |
| src/main.rs | N/A (entry point) | command_dispatch | MODIFY -- wire `clean` command |

## Unchanged Files

| File | Reason No Change Needed |
|------|-------------------------|
| src/config.rs | `RetentionConfig` already has `backups_to_keep_local` and `backups_to_keep_remote` fields. No new config params needed. |
| src/lock.rs | `lock_for_command("clean", ...)` already returns `LockScope::Global`. No change needed. |
| src/error.rs | Using `anyhow::Result` throughout; no new error variants required. |

## CLAUDE.md Tasks to Generate

1. **Update:** src/server/CLAUDE.md -- add documentation for:
   - New `clean` endpoint (replacing stub)
   - Retention/GC API operations

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **Retention logic**: Owned by `list.rs` (centralized listing and deletion)
- **API routes**: Owned by `server/routes.rs` (HTTP handlers)
- **CLI dispatch**: Owned by `main.rs` (command dispatch)
- **Config**: Owned by `config.rs` (already complete for retention)
- **Locking**: Owned by `lock.rs` (already correct for retention commands)

### What This Plan CANNOT Do
- Cannot add manifest caching (requires persistent state across server requests -- deferred to future optimization)
- Cannot test GC with real S3 in unit tests (integration test only)
- Cannot parallelize retention with create_remote on different hosts (design doc acknowledges this as a known race; mitigated by global PID lock on single host)

### Data Flow for Retention

```
retention_local:
  Config -> effective_retention_local() -> list_local() -> sort by timestamp ->
  filter !is_broken -> delete oldest exceeding count via delete_local()

retention_remote:
  Config -> effective_retention_remote() -> list_remote() -> sort by timestamp ->
  filter !is_broken -> for each to-delete:
    gc_collect_referenced_keys(surviving) -> gc_delete_backup(name, referenced_keys)

clean_shadow:
  ChClient.get_disks() -> walk shadow dirs -> filter by chbackup_* prefix ->
  check PID lock -> remove directories -> return count
```
