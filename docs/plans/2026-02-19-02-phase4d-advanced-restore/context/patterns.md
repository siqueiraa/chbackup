# Pattern Discovery

## Global Registry
No `docs/patterns/` directory exists. Full local discovery performed.

## Component Identification

### Files to Modify
1. `src/restore/mod.rs` -- Main orchestrator (add Mode A flow, pass `--rm` + config flags)
2. `src/restore/schema.rs` -- Add `drop_tables()`, ON CLUSTER DDL, DatabaseReplicated detection
3. `src/restore/remap.rs` -- Add `rewrite_distributed_cluster()` for config-driven cluster rewrite
4. `src/restore/topo.rs` -- Add reverse-priority sorting for DROP ordering
5. `src/restore/attach.rs` -- Add pending mutation re-apply after ATTACH
6. `src/clickhouse/client.rs` -- Add new query methods: `query_database_engine()`, `query_zookeeper_replica()`, `drop_replica_from_zkpath()`, `detach_table()`, `attach_table()`, `system_restore_replica()`
7. `src/config.rs` -- No changes needed (fields already exist)
8. `src/main.rs` -- Wire `--rm` flag to restore function (currently warns and ignores)
9. `src/cli.rs` -- No changes needed (--rm, --schema, --data-only already defined)

### New Functions/Methods Needed
- `ChClient::query_database_engine(db) -> Result<String>` -- query `system.databases` for engine name
- `ChClient::query_zookeeper_replica(zk_path, replica_name) -> Result<bool>` -- check `system.zookeeper`
- `ChClient::drop_replica_from_zkpath(replica_name, zk_path) -> Result<()>` -- `SYSTEM DROP REPLICA`
- `ChClient::detach_table(db, table) -> Result<()>` -- `DETACH TABLE ... SYNC`
- `ChClient::attach_table(db, table) -> Result<()>` -- `ATTACH TABLE`
- `ChClient::system_restore_replica(db, table) -> Result<()>` -- `SYSTEM RESTORE REPLICA`
- `drop_tables()` in schema.rs -- DROP in reverse engine priority
- `parse_replicated_params(ddl) -> Option<(String, String)>` -- extract ZK path + replica from DDL
- `resolve_zk_macros(path, macros) -> String` -- substitute {database}, {table}, {shard}, {replica}, {uuid}

## Existing Patterns Analyzed

### Pattern 1: DDL Execution (schema.rs)
```
1. Check if target exists (ch.table_exists / ch.database_exists)
2. Build DDL string (with IF NOT EXISTS safety)
3. Apply remap rewrites if active
4. ch.execute_ddl(&ddl)
5. Log result
```

### Pattern 2: ChClient Query Methods (client.rs)
```
1. Build SQL string with format!()
2. Conditional logging (log_sql_queries)
3. inner.query(&sql).fetch_all::<RowType>() or fetch_one
4. .context() error annotation
5. Return Result<Vec<T>> or Result<T>
```

### Pattern 3: Phased Restore Flow (mod.rs)
```
Phase 1: create_databases() -- sequential
Phase 2: create_tables() + parallel attach -- bounded by semaphore
Phase 2b: create_tables() for postponed -- sequential
Phase 3: create_ddl_objects() with retry -- sequential
Phase 4: create_functions() -- sequential
```

### Pattern 4: Table Classification (topo.rs)
```
classify_restore_tables() splits tables by:
  - Streaming engine -> postponed
  - Refreshable MV -> postponed
  - metadata_only -> ddl_only
  - else -> data
Sorted by priority functions.
```

### Pattern 5: Remap DDL Rewriting (remap.rs)
```
Pure functions, no async/IO:
  - String manipulation (find/replace, no regex crate)
  - Each transformation is a separate function
  - Compose in rewrite_create_table_ddl()
```
