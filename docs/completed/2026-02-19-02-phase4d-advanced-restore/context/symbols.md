# Type Verification Table

## Existing Types (Verified via Source)

| Variable/Field | Assumed Type | Actual Type | Verification Location |
|---|---|---|---|
| `config.clickhouse.restore_as_attach` | `bool` | `bool` | src/config.rs:154 |
| `config.clickhouse.restore_schema_on_cluster` | `String` | `String` | src/config.rs:158 |
| `config.clickhouse.restore_distributed_cluster` | `String` | `String` | src/config.rs:162 |
| `config.clickhouse.default_replica_path` | `String` | `String` | src/config.rs (verified via grep) |
| `config.clickhouse.default_replica_name` | `String` | `String` | src/config.rs (verified via grep) |
| `TableManifest.ddl` | `String` | `String` | src/manifest.rs:88 |
| `TableManifest.uuid` | `Option<String>` | `Option<String>` | src/manifest.rs:92 |
| `TableManifest.engine` | `String` | `String` | src/manifest.rs:96 |
| `TableManifest.pending_mutations` | `Vec<MutationInfo>` | `Vec<MutationInfo>` | src/manifest.rs:108 |
| `TableManifest.metadata_only` | `bool` | `bool` | src/manifest.rs:112 |
| `MutationInfo.mutation_id` | `String` | `String` | src/manifest.rs:176 |
| `MutationInfo.command` | `String` | `String` | src/manifest.rs:179 |
| `MutationInfo.parts_to_do` | `Vec<String>` | `Vec<String>` | src/manifest.rs:183 |
| `DatabaseInfo.name` | `String` | `String` | src/manifest.rs:165 |
| `DatabaseInfo.ddl` | `String` | `String` | src/manifest.rs:169 |
| `BackupManifest.databases` | `Vec<DatabaseInfo>` | `Vec<DatabaseInfo>` | src/manifest.rs:69 |
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | src/manifest.rs:65 |
| `RemapConfig.rename_as` | `Option<(String,String,String,String)>` | `Option<(String,String,String,String)>` | src/restore/remap.rs:14 |
| `RemapConfig.database_mapping` | `HashMap<String, String>` | `HashMap<String, String>` | src/restore/remap.rs:18 |
| `RemapConfig.default_replica_path` | `String` | `String` | src/restore/remap.rs:20 |
| `RestorePhases.data_tables` | `Vec<String>` | `Vec<String>` | src/restore/topo.rs:77 |
| `RestorePhases.postponed_tables` | `Vec<String>` | `Vec<String>` | src/restore/topo.rs:79 |
| `RestorePhases.ddl_only_tables` | `Vec<String>` | `Vec<String>` | src/restore/topo.rs:81 |
| `OwnedAttachParams.engine` | `String` | `String` | src/restore/attach.rs:77 |
| `ChClient (Clone)` | implements Clone | `#[derive(Clone)]` | src/clickhouse/client.rs:14 |

## New Types to Add (Planned)

| New Type/Method | Location | Purpose |
|---|---|---|
| `ChClient::query_database_engine(db) -> Result<String>` | src/clickhouse/client.rs | Query system.databases for engine name (detect DatabaseReplicated) |
| `ChClient::query_zookeeper_replica(zk_path, replica) -> Result<bool>` | src/clickhouse/client.rs | Check system.zookeeper for existing replica |
| `ChClient::drop_replica_from_zkpath(replica, zk_path) -> Result<()>` | src/clickhouse/client.rs | SYSTEM DROP REPLICA FROM ZKPATH |
| `ChClient::detach_table_sync(db, table) -> Result<()>` | src/clickhouse/client.rs | DETACH TABLE SYNC |
| `ChClient::attach_table(db, table) -> Result<()>` | src/clickhouse/client.rs | ATTACH TABLE |
| `ChClient::system_restore_replica(db, table) -> Result<()>` | src/clickhouse/client.rs | SYSTEM RESTORE REPLICA |
| `ChClient::drop_table(db, table) -> Result<()>` | src/clickhouse/client.rs | DROP TABLE IF EXISTS |

## Verified Method Existence

| Method | Exists? | File:Line |
|---|---|---|
| `ChClient::execute_ddl()` | YES | src/clickhouse/client.rs:453 |
| `ChClient::table_exists()` | YES | src/clickhouse/client.rs:516 |
| `ChClient::database_exists()` | YES | src/clickhouse/client.rs:494 |
| `ChClient::get_macros()` | YES | src/clickhouse/client.rs:421 |
| `ChClient::attach_part()` | YES | src/clickhouse/client.rs:368 |
| `ChClient::list_tables()` | YES | src/clickhouse/client.rs:262 |
| `ChClient::get_disks()` | YES | src/clickhouse/client.rs:396 |
| `create_tables()` | YES | src/restore/schema.rs:113 |
| `create_databases()` | YES | src/restore/schema.rs:26 |
| `create_ddl_objects()` | YES | src/restore/schema.rs:198 |
| `create_functions()` | YES | src/restore/schema.rs:319 |
| `classify_restore_tables()` | YES | src/restore/topo.rs:92 |
| `topological_sort()` | YES | src/restore/topo.rs:136 |
| `rewrite_create_table_ddl()` | YES | src/restore/remap.rs:193 |
| `rewrite_create_database_ddl()` | YES | src/restore/remap.rs:227 |
| `engine_restore_priority()` | YES | src/restore/topo.rs:64 |
| `data_table_priority()` | YES | src/restore/topo.rs:21 |
| `is_streaming_engine()` | YES | src/restore/topo.rs:39 |
| `is_refreshable_mv()` | YES | src/restore/topo.rs:51 |
