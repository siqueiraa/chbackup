# Data Authority Analysis

## Data Requirements and Source Verification

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Pending mutations | `TableManifest` | `pending_mutations: Vec<MutationInfo>` (mutation_id, command, parts_to_do) | USE EXISTING -- manifest already stores pending mutations from backup |
| Table engine name | `TableManifest` | `engine: String` | USE EXISTING -- engine stored in manifest |
| Table DDL | `TableManifest` | `ddl: String` | USE EXISTING -- full CREATE TABLE DDL in manifest |
| Table UUID | `TableManifest` | `uuid: Option<String>` | USE EXISTING -- UUID from system.tables at backup time |
| Database engine | `system.databases` (live query) | N/A -- not in manifest | MUST IMPLEMENT -- need `query_database_engine()` to detect DatabaseReplicated at restore time |
| ZK path + replica name | Parsed from DDL | First two single-quoted args in Replicated*MergeTree(...) | MUST IMPLEMENT -- need `parse_replicated_params()` to extract ZK path and replica name |
| ZK macro values | `ChClient::get_macros()` | `HashMap<String, String>` from system.macros | USE EXISTING -- get_macros() already returns all macro substitutions |
| ZK replica existence | `system.zookeeper` (live query) | N/A -- not queryable from manifest | MUST IMPLEMENT -- need `query_zookeeper_replica()` to check if replica path exists |
| Database DDL | `DatabaseInfo` | `ddl: String` | USE EXISTING -- CREATE DATABASE DDL stored in manifest |
| Table dependencies | `TableManifest` | `dependencies: Vec<String>` | USE EXISTING -- dependencies stored in manifest |
| Table metadata_only | `TableManifest` | `metadata_only: bool` | USE EXISTING -- flag stored in manifest |
| Config: restore_as_attach | `Config` | `config.clickhouse.restore_as_attach: bool` | USE EXISTING -- config field exists |
| Config: restore_schema_on_cluster | `Config` | `config.clickhouse.restore_schema_on_cluster: String` | USE EXISTING -- config field exists |
| Config: restore_distributed_cluster | `Config` | `config.clickhouse.restore_distributed_cluster: String` | USE EXISTING -- config field exists |
| Config: default_replica_path | `Config` | `config.clickhouse.default_replica_path: String` | USE EXISTING -- config field exists |
| Config: default_replica_name | `Config` | `config.clickhouse.default_replica_name: String` | USE EXISTING -- config field exists |
| Existing ZK path rewriting | `remap.rs` | `rewrite_replicated_zk_path()` (private fn) | USE EXISTING (partially) -- already parses ZK path from DDL for rewriting; new `parse_replicated_params()` needs different output (returns both path and replica, not rewritten DDL) |
| ON CLUSTER clause injection | N/A | N/A | MUST IMPLEMENT -- pure string manipulation to add ON CLUSTER to DDL |
| Distributed cluster rewriting | `remap.rs` | `rewrite_distributed_engine()` (private fn) | USE EXISTING (partially) -- already rewrites Distributed engine args; needs extension for cluster name rewrite |
| Reverse engine priority for DROP | `topo.rs` | `engine_restore_priority()` returns u8 | USE EXISTING (partially) -- can reverse the priority for DROP ordering |

## Analysis Notes

- **MutationInfo is fully populated during backup**: The `check_pending_mutations()` method in ChClient already queries `system.mutations` and populates `TableManifest.pending_mutations` during backup creation. The data is available in the manifest for re-application.

- **MutationInfo.command format**: The `command` field contains the mutation SQL like "DELETE WHERE id = 5" or "UPDATE x = 1 WHERE id = 5". Design doc section 5.7 says to re-apply with `ALTER TABLE {db}.{table} {command} SETTINGS mutations_sync=2`.

- **Database engine detection must be live**: The manifest's `DatabaseInfo` only has `name` and `ddl` -- it does NOT store the engine. Detecting `DatabaseReplicated` requires querying the LIVE ClickHouse instance's `system.databases` table. This is correct because the target server may have a different database engine than the source.

- **ZK path parsing already exists in remap.rs**: The private function `rewrite_replicated_zk_path()` already locates the ZK path in Replicated*MergeTree DDL. However, Phase 4d needs a different function that EXTRACTS the path and replica name (returns them) rather than REWRITING the DDL. Consider factoring out the parsing logic.

- **ON CLUSTER must be skipped for DatabaseReplicated**: Design doc section 5.10 says DatabaseReplicated databases automatically replicate DDL via Keeper, so ON CLUSTER must not be added for those databases. This requires the live database engine query.

- **Config fields already have defaults**: All three restore config fields default to "off" values (false/empty), so Phase 4d can be added without breaking existing behavior.

## Summary

| Category | Count |
|----------|-------|
| USE EXISTING | 14 |
| USE EXISTING (partially) | 3 |
| MUST IMPLEMENT | 3 |

### Must Implement Justifications

1. **`query_database_engine()`** -- No existing source provides database engine info; manifest only stores DDL text, not parsed engine name. Must query live system.databases.

2. **`parse_replicated_params()`** -- Existing `rewrite_replicated_zk_path()` rewrites DDL inline but does not return extracted values. Need a separate extraction function that returns `Option<(String, String)>` for (zk_path, replica_name).

3. **`query_zookeeper_replica()`** -- ZooKeeper state is runtime-only; no way to know if a replica path exists without querying `system.zookeeper` on the target server.
