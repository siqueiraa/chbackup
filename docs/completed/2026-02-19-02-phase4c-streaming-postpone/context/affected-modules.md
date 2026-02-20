# Affected Modules Analysis

## Summary

- **Modules to update:** 1
- **Modules to create:** 0
- **Git base:** e0668461

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action | Files |
|--------|------------------|----------|--------|-------|
| src/restore | EXISTS | new_patterns | UPDATE | topo.rs, mod.rs, schema.rs |

## CLAUDE.md Tasks to Generate

1. **Update:** src/restore/CLAUDE.md -- Add Phase 2b postponed table handling, streaming engine detection functions, refreshable MV detection

## Files Modified (Detailed)

### src/restore/topo.rs
- Add `is_streaming_engine()` function
- Add `is_refreshable_mv()` function
- Modify `classify_restore_tables()` to populate `postponed_tables`
- Add unit tests for new classification logic

### src/restore/mod.rs
- Add Phase 2b step between data attachment and Phase 3
- Call `create_tables()` for postponed tables after data is attached
- Add logging for Phase 2b

### src/restore/schema.rs
- No structural changes needed -- `create_tables()` already works for any table list
- May need minor adjustments if postponed tables need special handling (unlikely)

## Non-Modified Modules (Context Only)

| Module | Relevance |
|--------|-----------|
| src/backup | Contains `is_metadata_only_engine()` -- reference pattern only, not modified |
| src/table_filter | Contains `is_engine_excluded()` -- reference pattern only, not modified |
| src/manifest | Contains `TableManifest` struct -- not modified, only read |
