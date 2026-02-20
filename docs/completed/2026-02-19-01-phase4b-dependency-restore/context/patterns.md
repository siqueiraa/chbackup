# Pattern Discovery

No global `docs/patterns/` directory exists. Full pattern discovery performed locally.

## Component Identification

### Components Needed for Phase 4b

| Component | Type | Exists? | Location |
|-----------|------|---------|----------|
| BackupManifest | struct | YES | src/manifest.rs:19 |
| TableManifest | struct | YES | src/manifest.rs:86 |
| TableManifest.dependencies | field | YES (always empty) | src/manifest.rs:116 |
| TableRow | struct | YES | src/clickhouse/client.rs:25 |
| list_tables() | method | YES | src/clickhouse/client.rs:250 |
| list_tables_with_deps() | method | NO | Must add |
| create_tables() | fn | YES | src/restore/schema.rs:113 |
| is_metadata_only_engine() | fn | YES | src/backup/mod.rs:570 |
| restore() | fn | YES | src/restore/mod.rs:62 |
| ensure_if_not_exists_table() | fn | YES | src/restore/schema.rs:198 |
| Topological sort | logic | NO | Must add |
| Engine priority sort | logic | NO | Must add |
| Fallback retry loop | logic | NO | Must add |

## Reference Implementations Analyzed

### Pattern 1: ChClient Query Method (list_tables)

```
Location: src/clickhouse/client.rs:250-271
Pattern:
  1. Build SQL string with column selection from system.tables
  2. Conditional logging (log_sql_queries flag -> info vs debug)
  3. Execute via self.inner.query(sql).fetch_all::<RowType>()
  4. .context() for error wrapping
  5. Return Result<Vec<RowType>>
```

Key: New `list_tables_with_deps()` must follow this pattern exactly. The query adds `dependencies_database` and `dependencies_table` columns, which are `Array(String)` in ClickHouse.

### Pattern 2: Backup Table Manifest Construction

```
Location: src/backup/mod.rs:215-252
Pattern:
  1. Iterate filtered_tables
  2. Check is_metadata_only_engine(engine)
  3. For metadata-only: insert TableManifest with metadata_only=true, dependencies=Vec::new()
  4. For data tables: push to data_tables vec for FREEZE phase
```

Key: Dependencies are currently always `Vec::new()`. Must be populated by combining `dependencies_database[i]` + "." + `dependencies_table[i]` from the CH query result.

### Pattern 3: Restore Schema Creation (create_tables)

```
Location: src/restore/schema.rs:113-186
Pattern:
  1. For each table_key in table_keys (order determined by caller)
  2. Get TableManifest from manifest.tables
  3. Split table_key into (src_db, src_table)
  4. Determine (dst_db, dst_table) via remap
  5. Check if exists via ch.table_exists()
  6. Build DDL with ensure_if_not_exists_table()
  7. Execute DDL via ch.execute_ddl()
```

Key: Currently iterates `table_keys` in caller-provided order. The caller (restore::restore) collects keys from HashMap iteration order (arbitrary). Must add ordering logic.

### Pattern 4: Restore Flow Orchestration

```
Location: src/restore/mod.rs:62-432
Pattern:
  Phase 1: create_databases()
  Phase 2: create_tables() -- ALL tables in one pass, arbitrary order
  Phase 3: attach data parts (skips metadata_only)

  Current MISSING phases:
  - No separation of data tables vs DDL-only objects
  - No engine priority sorting
  - No topological sort
  - No Phase 3 (DDL-only) or Phase 4 (functions/named collections/RBAC)
```

## Architectural Pattern for the Plan

The restore flow needs to be restructured from the current:
```
create_databases -> create_tables(ALL) -> attach_parts(data only)
```
To the design doc's phased architecture:
```
Phase 1: create_databases
Phase 2: create_tables(data tables, engine-priority sorted) + attach_parts
Phase 2b: create_tables(streaming/refresh engines -- postponed)
Phase 3: create_tables(DDL-only objects, topo-sorted by dependencies)
Phase 4: functions, named_collections, RBAC (separate plan scope)
```

Phase 4b plan covers: Phase 2 ordering + Phase 3 (DDL-only with topo sort) + fallback retry loop.
Functions/Named Collections/RBAC restore is Phase 4e scope per roadmap.
