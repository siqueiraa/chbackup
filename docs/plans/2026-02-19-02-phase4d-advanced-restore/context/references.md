# References and Symbol Analysis

## Phase 4d Scope

Phase 4d adds Mode A restore (`--rm`), ATTACH TABLE mode, ZK path conflict resolution, ON CLUSTER restore, DatabaseReplicated detection, Distributed table cluster fix, and pending mutation re-application.

## Key Function Signatures (Verified via LSP)

### restore::restore() -- Main Entry Point
```rust
// src/restore/mod.rs:63
pub async fn restore(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    data_only: bool,
    resume: bool,
    rename_as: Option<&str>,
    database_mapping: Option<&HashMap<String, String>>,
) -> Result<()>
```
**Callers (5):**
- `src/main.rs:263` -- CLI Restore command
- `src/main.rs:385` -- CLI RestoreRemote command
- `src/server/routes.rs:570` -- POST /api/v1/restore handler
- `src/server/routes.rs:807` -- POST /api/v1/restore_remote handler
- `src/server/state.rs:386` -- auto_resume() function

**Impact:** Adding `rm: bool` parameter requires updating all 5 call sites.

### schema::create_databases()
```rust
// src/restore/schema.rs:26
pub async fn create_databases(
    ch: &ChClient,
    manifest: &BackupManifest,
    remap: Option<&RemapConfig>,
) -> Result<()>
```
**Callers:** `src/restore/mod.rs:149` only.
**Impact:** Mode A needs DROP DATABASE IF EXISTS before CREATE. May need `rm: bool` parameter or separate `drop_databases()` function.

### schema::create_tables()
```rust
// src/restore/schema.rs:113
pub async fn create_tables(
    ch: &ChClient,
    manifest: &BackupManifest,
    table_keys: &[String],
    data_only: bool,
    remap: Option<&RemapConfig>,
) -> Result<()>
```
**Callers:**
- `src/restore/mod.rs:158` -- Phase 2 data tables
- `src/restore/mod.rs:180` -- Phase 2b postponed tables
- `src/restore/mod.rs:461` -- Phase 2b in schema-only mode

**Impact:** Mode A needs DROP TABLE IF EXISTS before CREATE, plus ZK conflict resolution for Replicated tables. Will need `rm: bool` or separate `drop_tables()` function.

### topo::classify_restore_tables()
```rust
// src/restore/topo.rs:92
pub fn classify_restore_tables(
    manifest: &BackupManifest,
    table_keys: &[String],
) -> RestorePhases
```
**Callers:** `src/restore/mod.rs:145` and 5 test functions in topo.rs
**Impact:** No signature change needed, but will need new `reverse_engine_priority()` function for DROP ordering.

### topo::engine_restore_priority()
```rust
// src/restore/topo.rs:71
pub fn engine_restore_priority(engine: &str) -> u8
```
Dictionary=0, View/MV/LiveView/WindowView=1, Distributed/Merge=2, other=3
**Impact:** Mode A DROP needs reverse order (Distributed first=0, Views=1, Dictionary=2, data=3).

## Existing Config Fields (Verified in config.rs)

All config fields needed for Phase 4d **already exist**:

| Field | Type | Default | Location |
|-------|------|---------|----------|
| `restore_as_attach` | `bool` | `false` | config.rs ClickHouseConfig |
| `restore_schema_on_cluster` | `String` | `""` | config.rs ClickHouseConfig |
| `restore_distributed_cluster` | `String` | `""` | config.rs ClickHouseConfig |
| `default_replica_path` | `String` | `/clickhouse/tables/{shard}/{database}/{table}` | config.rs ClickHouseConfig |
| `default_replica_name` | `String` | `{replica}` | config.rs ClickHouseConfig |
| `backup_mutations` | `bool` | `true` | config.rs BackupConfig |

## Existing Manifest Types (Verified in manifest.rs)

```rust
// src/manifest.rs
pub struct MutationInfo {
    pub mutation_id: String,
    pub command: String,
    pub parts_to_do: Vec<String>,
}

// In TableManifest:
pub pending_mutations: Vec<MutationInfo>,

// In DatabaseInfo:
pub struct DatabaseInfo {
    pub name: String,
    pub ddl: String,
}
```

## CLI Flags (Verified in cli.rs)

```rust
// src/cli.rs - Restore command
#[arg(long, visible_alias = "drop")]
pub rm: bool,
// Also: --schema, --data-only, --resume, --as, -m flags

// src/cli.rs - RestoreRemote command
#[arg(long, visible_alias = "drop")]
pub rm: bool,
```

Both `--rm` flags are defined and captured but currently warn "not yet implemented" in main.rs.

## Server Request Types (Verified in routes.rs)

```rust
// src/server/routes.rs:618
pub struct RestoreRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    pub rename_as: Option<String>,
    pub database_mapping: Option<String>,
    pub rm: Option<bool>,        // Already exists!
}

// src/server/routes.rs:855
pub struct RestoreRemoteRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    pub rename_as: Option<String>,
    pub database_mapping: Option<String>,
    // NOTE: RestoreRemoteRequest does NOT have rm field yet
}
```

## Missing ChClient Methods (Need to be Added)

The following methods do NOT exist in `src/clickhouse/client.rs` and must be implemented:

1. **`drop_table(db, table) -> Result<()>`** -- `DROP TABLE IF EXISTS \`db\`.\`table\` [ON CLUSTER 'cluster'] SYNC`
2. **`drop_database(db) -> Result<()>`** -- `DROP DATABASE IF EXISTS \`db\` [ON CLUSTER 'cluster'] SYNC`
3. **`detach_table(db, table) -> Result<()>`** -- `DETACH TABLE \`db\`.\`table\` SYNC` (for ATTACH TABLE mode)
4. **`attach_table(db, table) -> Result<()>`** -- `ATTACH TABLE \`db\`.\`table\`` (full table, not part)
5. **`drop_replica(replica_name, zk_path) -> Result<()>`** -- `SYSTEM DROP REPLICA 'name' FROM ZKPATH 'path'`
6. **`restore_replica(db, table) -> Result<()>`** -- `SYSTEM RESTORE REPLICA \`db\`.\`table\``
7. **`query_database_engine(db) -> Result<String>`** -- `SELECT engine FROM system.databases WHERE name = '{db}'`
8. **`check_zk_replica_exists(zk_path, replica_name) -> Result<bool>`** -- `SELECT count() FROM system.zookeeper WHERE path='{path}/replicas/{name}'`
9. **`execute_mutation(db, table, command) -> Result<()>`** -- Execute mutation command (ALTER TABLE ... DELETE/UPDATE WHERE ...)

## Existing DDL Rewriting (remap.rs)

The following rewrite functions already exist and can be reused/extended:

| Function | Purpose | Extends For |
|----------|---------|-------------|
| `rewrite_create_table_ddl()` | 4 transforms: name, UUID, ZK path, Distributed | Distributed cluster rewrite |
| `rewrite_replicated_zk_path()` | Parse + rewrite ZK path in Replicated engine | ZK conflict detection (parse only) |
| `rewrite_create_database_ddl()` | Rewrite database name | ON CLUSTER clause injection |
| `rewrite_distributed_engine()` | Update db/table in Distributed engine | Cluster name rewrite |

### ZK Path Parsing (Already Implemented in remap.rs)

`rewrite_replicated_zk_path()` already parses `ReplicatedMergeTree('/path', 'replica')` from DDL. This can be refactored into:
1. `parse_replicated_params(ddl) -> Option<(String, String)>` -- extract (zk_path, replica_name)
2. `resolve_macros(template, macros: &HashMap<String, String>) -> String` -- substitute {shard}, {replica}, etc.
3. The existing `get_macros()` ChClient method returns `HashMap<String, String>` from `system.macros`

## Design Doc References

| Section | Feature | Key Details |
|---------|---------|-------------|
| Section 5.2 | Mode A full restore | DROP in reverse engine priority, retry loop for dependency failures |
| Section 5.2 | Mode B non-destructive | Current implementation (IF NOT EXISTS) |
| Section 5.2 | ATTACH TABLE mode | DETACH SYNC -> DROP REPLICA -> ATTACH TABLE -> RESTORE REPLICA |
| Section 5.2 | ZK conflict resolution | Parse Replicated params, resolve macros, check system.zookeeper, DROP REPLICA |
| Section 5.1 | ON CLUSTER restore | Add ON CLUSTER clause to DDL; skip for DatabaseReplicated |
| Section 5.1 | DatabaseReplicated | Skip ON CLUSTER, regenerate UUIDs |
| Section 5.1 | Distributed cluster fix | Rewrite cluster name in Distributed engine DDL |
| Section 5.7 | Pending mutations | Re-apply mutations after ATTACH; warn + ALTER TABLE ... |

## RestorePhases Struct (topo.rs)
```rust
pub struct RestorePhases {
    pub data_tables: Vec<String>,
    pub ddl_only_tables: Vec<String>,
    pub postponed_tables: Vec<String>,
}
```

## OwnedAttachParams Struct Fields (attach.rs)
18 fields including: ch, db, table, engine, backup_dir, table_data_path, parts, disk_type_map, s3_client, disk_remote_paths, object_disk_server_side_copy_concurrency, allow_object_disk_streaming, ch_uid, ch_gid, already_attached, restore_state_path, log_sql_queries, data_path

**Impact:** May need extension for mutation re-application (pending_mutations field) or can be handled at the orchestrator level in mod.rs after all parts are attached.
