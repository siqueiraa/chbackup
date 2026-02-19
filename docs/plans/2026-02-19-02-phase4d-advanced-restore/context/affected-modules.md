# Affected Modules Analysis

## Summary

- **Modules to update:** 2
- **Modules to create:** 0
- **Top-level files modified:** 1 (src/main.rs)
- **Git base:** HEAD~10

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action |
|--------|------------------|----------|--------|
| src/restore | EXISTS | new_patterns | UPDATE |
| src/clickhouse | EXISTS | new_patterns | UPDATE |

## Top-Level Files

| File | Change |
|------|--------|
| src/main.rs | Wire `--rm` flag to `restore()` function (currently warns and ignores) |

## Files Modified Per Module

### src/restore (5 files)

| File | Changes |
|------|---------|
| mod.rs | Add `rm` parameter to `restore()`, Mode A flow (drop_tables before create), ATTACH TABLE mode flow, pass config flags |
| schema.rs | Add `drop_tables()`, `drop_databases()`, ON CLUSTER clause injection, DatabaseReplicated detection |
| remap.rs | Add `parse_replicated_params()`, `resolve_zk_macros()`, extend `rewrite_distributed_engine()` for cluster name |
| topo.rs | Add reverse engine priority sorting for DROP ordering |
| attach.rs | Add pending mutation re-apply after all parts attached |

### src/clickhouse (1 file)

| File | Changes |
|------|---------|
| client.rs | Add 7 new methods: `query_database_engine`, `query_zookeeper_replica`, `drop_replica_from_zkpath`, `detach_table_sync`, `attach_table`, `system_restore_replica`, `drop_table` |

## CLAUDE.md Tasks to Generate

1. **Update:** src/restore/CLAUDE.md (new patterns: Mode A, ATTACH TABLE mode, DROP ordering, mutation re-apply, ON CLUSTER, DatabaseReplicated)
2. **Update:** src/clickhouse/CLAUDE.md (new methods: 7 new ChClient query/command methods)
