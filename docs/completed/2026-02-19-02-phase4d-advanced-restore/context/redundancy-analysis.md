# Redundancy Analysis

## Proposed New Components

Phase 4d proposes adding the following new public functions/methods:

### New ChClient Methods (src/clickhouse/client.rs)

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| `ChClient::query_database_engine(db) -> Result<String>` | `ChClient::database_exists(db)` queries system.databases but only checks count, not engine | COEXIST | Different purpose: database_exists checks existence, query_database_engine returns engine name. Both query system.databases but with different SELECT columns. Cleanup: N/A -- both are genuinely distinct queries. |
| `ChClient::query_zookeeper_replica(zk_path, replica) -> Result<bool>` | None | N/A | No existing code queries system.zookeeper. |
| `ChClient::drop_replica_from_zkpath(replica, zk_path) -> Result<()>` | None | N/A | No existing SYSTEM DROP REPLICA code. |
| `ChClient::detach_table_sync(db, table) -> Result<()>` | None | N/A | No existing DETACH TABLE code. |
| `ChClient::attach_table(db, table) -> Result<()>` | `ChClient::attach_part(db, table, part_name)` | COEXIST | Different SQL: attach_part is `ALTER TABLE ATTACH PART`, attach_table is `ATTACH TABLE` (whole table). Genuinely distinct operations. |
| `ChClient::system_restore_replica(db, table) -> Result<()>` | None | N/A | No existing SYSTEM RESTORE REPLICA code. |
| `ChClient::drop_table(db, table) -> Result<()>` | `ChClient::drop_integration_tables()` uses `execute_ddl("DROP TABLE IF EXISTS ...")` inline | COEXIST | drop_integration_tables drops specific hardcoded tables. A generic `drop_table(db, table)` method is needed for arbitrary tables. Could refactor drop_integration_tables to use drop_table, but that is cosmetic and not required. |

### New Restore Functions (src/restore/)

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| `drop_tables()` in schema.rs | None | N/A | No existing DROP TABLE orchestration in restore module. |
| `parse_replicated_params(ddl) -> Option<(String, String)>` in remap.rs | `rewrite_replicated_zk_path()` (private, remap.rs:330) | EXTEND | The existing private function parses the same ZK path location but rewrites DDL inline instead of returning extracted values. Factor out the parsing logic into a shared helper, keep rewrite_replicated_zk_path as a caller. No removal needed -- this is a refactor to share code. |
| `resolve_zk_macros(path, macros) -> String` in remap.rs | `rewrite_replicated_zk_path()` does `replace("{database}", ...)` and `replace("{table}", ...)` but not full macro resolution | EXTEND | Existing code only substitutes {database} and {table}. Phase 4d needs {shard}, {replica}, {uuid} and values from system.macros. Extend the replacement logic. |
| `rewrite_distributed_cluster()` in remap.rs | `rewrite_distributed_engine()` (private, remap.rs:375) | EXTEND | Existing private function rewrites database/table args of Distributed engine. Phase 4d needs to also rewrite the CLUSTER arg (first positional arg). Extend the existing function. |
| `add_on_cluster_clause(ddl, cluster) -> String` in schema.rs or remap.rs | None | N/A | No existing ON CLUSTER injection code. |

### Restore Flow Changes (src/restore/mod.rs)

| Proposed | Existing Match | Decision | Justification |
|----------|---------------|----------|---------------|
| Mode A flow (DROP + CREATE + restore) in restore() | Mode B flow in restore() | EXTEND | Adding a new code path within the existing restore() function. Not a new function. |
| ATTACH TABLE mode flow in restore() | Part attachment flow in restore() | EXTEND | Adding an alternative code path when `restore_as_attach: true`. Not a new function. |
| Pending mutation re-apply after ATTACH | None | N/A | No existing mutation re-application code in restore module. |

## Summary

- **N/A (truly new)**: 8 components
- **EXTEND (add to existing)**: 4 components
- **COEXIST (both needed)**: 3 components
- **REPLACE**: 0 components
- **REUSE**: 0 components

No REPLACE or removal tasks needed. All COEXIST items have clear justification for distinct purposes.
