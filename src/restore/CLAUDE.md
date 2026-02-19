# CLAUDE.md -- src/restore

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `restore` command -- reads a backup manifest, creates databases/tables from DDL, hardlinks data parts to ClickHouse's `detached/` directory, and executes `ALTER TABLE ATTACH PART`.

Phase 1 implements Mode B only (non-destructive). Mode A (`--rm` DROP) is Phase 4d.

## Directory Structure

```
src/restore/
  mod.rs      -- Entry point: restore() orchestrates phased restore flow
  topo.rs     -- Table classification, topological sort, engine priority, streaming engine detection (Phase 4b+4c)
  remap.rs    -- Table/database remap: DDL rewriting, name mapping for --as and -m flags
  schema.rs   -- CREATE DATABASE, CREATE TABLE, create_ddl_objects, create_functions (remap-aware)
  attach.rs   -- Hardlink parts to detached/, chown, ATTACH PART
  sort.rs     -- SortPartsByMinBlock for correct attachment order
```

## Key Patterns

### Part Sort Order (sort.rs)
Parts are sorted by `(partition, min_block)` for correct merge behavior. Part name format is `{partition}_{min}_{max}_{level}` where partition may contain underscores, so parsing splits from the right. Engines containing "Replacing", "Collapsing", or "Versioned" require strictly sequential sorted ATTACH.

### DDL Rewriting / Remap (remap.rs, Phase 4a)
All functions are pure (no async, no I/O) for easy unit testing. DDL rewriting uses string manipulation (no regex crate dependency).

- **`RemapConfig`** struct: Holds parsed remap state from CLI flags. Fields: `rename_as` (single table rename as `(src_db, src_table, dst_db, dst_table)`), `database_mapping` (HashMap of src_db -> dst_db), `default_replica_path` (ZK path template from config). Created via `RemapConfig::new()` which validates `--as` requires `-t` (single table, no wildcards).
- **`RemapConfig::remap_table_key(original_key)`**: Given a manifest key `"db.table"`, returns the destination `(db, table)`. Priority: `--as` rename takes precedence over `-m` database mapping. Non-matched keys pass through unchanged.
- **`parse_database_mapping(s)`**: Parses `-m prod:staging,logs:logs_copy` format into `HashMap<String, String>`. Validates colon separator and non-empty source/destination. Returns `Result`.
- **`rewrite_create_table_ddl()`**: Applies four transformations to CREATE TABLE DDL:
  1. Table name replacement (handles both backtick-quoted and unquoted `db.table`)
  2. UUID clause removal (`UUID 'hex-hex-hex'` -> empty, lets ClickHouse assign new)
  3. ZK path rewriting in ReplicatedMergeTree engine (first single-quoted arg replaced with `default_replica_path` template substituting `{database}` and `{table}`)
  4. Distributed engine database/table reference update (second and third positional args)
- **`rewrite_create_database_ddl()`**: Rewrites database name in `CREATE DATABASE` DDL (backtick-quoted and unquoted)

### Phased Restore Architecture (Phase 4b+4c, mod.rs)
The restore flow is structured into explicit phases per design doc 5.1/5.5/5.6:
```
Phase 1:  CREATE databases           (create_databases)
Phase 2:  CREATE + ATTACH data tables (sorted by engine priority via classify_restore_tables)
Phase 2b: CREATE postponed tables     (streaming engines, refreshable MVs -- Phase 4c)
Phase 3:  CREATE DDL-only objects     (topologically sorted by dependencies via topological_sort + create_ddl_objects)
Phase 4:  CREATE functions            (from manifest.functions via create_functions)
```
- `classify_restore_tables()` splits filtered table keys into `RestorePhases` (data_tables, postponed_tables, ddl_only_tables)
- Phase 2 passes `phases.data_tables` to `create_tables()` and the data attach loop (instead of all table_keys)
- Phase 2b creates postponed tables (streaming engines + refreshable MVs) AFTER all data is attached but BEFORE DDL-only objects. This prevents streaming engines from consuming data prematurely during restore.
- Phase 3 runs `topological_sort()` on `phases.ddl_only_tables`, then `create_ddl_objects()` on the sorted result
- Phase 4 calls `create_functions()` for manifest.functions DDL
- Schema-only mode (`--schema-only`) creates all schema (Phases 1-4) but skips data attach. Phase 2b runs AFTER Phase 3 DDL-only objects (since DDL-only objects like regular MVs may be targets that streaming engines write to).
- Data-only mode (`--data-only`) skips Phases 1, 2b, 3, and 4
- Resume state only queries `system.parts` for data tables (DDL-only and postponed objects have no parts)

### Table Classification and Topological Sort (topo.rs)
- **`RestorePhases`** struct: `data_tables: Vec<String>`, `ddl_only_tables: Vec<String>`, `postponed_tables: Vec<String>`
- **`classify_restore_tables(manifest, table_keys)`**: Splits tables using a priority decision tree: (1) streaming engine -> postponed, (2) refreshable MV -> postponed, (3) metadata_only -> ddl_only, (4) else -> data. Data tables sorted by `data_table_priority()` (regular=0, `.inner` tables=1). Logs classification counts including postponed.
- **`is_streaming_engine(engine)`**: Returns true for Kafka, NATS, RabbitMQ, S3Queue. These engines consume data from external sources and must not be created until data tables are fully attached.
- **`is_refreshable_mv(tm)`**: Returns true if `tm.engine == "MaterializedView"` AND DDL contains the `REFRESH` keyword (case-insensitive, preceded by whitespace or newline). Refreshable MVs run scheduled queries that should not execute against incomplete data.
- **`data_table_priority(table_key)`**: Returns 0 for regular tables, 1 for `.inner`/`.inner_id` tables (MV storage targets created before MVs)
- **`engine_restore_priority(engine)`**: Per design doc 5.1: Dictionary=0, View/MaterializedView/LiveView/WindowView=1, Distributed/Merge=2, other=3
- **`topological_sort(tables, keys)`**: Kahn's algorithm with engine-priority tie-breaking. If no tables have dependencies (CH < 23.3 or old manifest), falls back to engine-priority-only sorting. Detects cycles by checking for remaining nodes with non-zero in-degree; appends cyclic nodes in engine-priority order with a warning log.

### Streaming Engine Postponement (Phase 4c, topo.rs + mod.rs)
Streaming engines (Kafka, NATS, RabbitMQ, S3Queue) and refreshable materialized views are postponed to Phase 2b to prevent premature data consumption during restore. The classification decision tree in `classify_restore_tables()` checks streaming engine and refreshable MV status BEFORE the `metadata_only` check, ensuring these tables are routed to `postponed_tables` regardless of their `metadata_only` flag.

Key behaviors:
- Streaming engines are identified by exact engine name match via `is_streaming_engine()` using `matches!()` macro
- Refreshable MVs are identified by engine == "MaterializedView" AND case-insensitive detection of ` REFRESH ` or `\nREFRESH ` in the DDL string (whitespace/newline boundary prevents false positives from column names)
- In full restore mode: Phase 2b runs after data attachment, before Phase 3 DDL-only objects
- In schema-only mode: Phase 2b runs after Phase 3 DDL-only objects (since those may be targets streaming engines write to)
- In data-only mode: Phase 2b is skipped (guarded by `!data_only` check; `create_tables()` also has internal `data_only` guard)
- Phase 2b reuses the existing `create_tables()` function -- no special DDL handling needed

### Schema Creation (schema.rs)
- `create_databases()`: Executes DDL from `manifest.databases[]` with `IF NOT EXISTS` safety. When remap is active, creates target databases with rewritten DDL; tracks created databases to avoid duplicates.
- `create_tables()`: For each filtered table, checks existence first, then executes the stored `CREATE TABLE` DDL. When remap is active, rewrites DDL before execution and checks existence using destination db/table names.
- `create_ddl_objects()`: Phase 3 DDL-only object creation with retry-loop fallback. Creates objects sequentially in caller-provided (topologically sorted) order. On failure, queues for retry (max 10 rounds). Each round retries failed objects; if zero progress in a round after round 0, bails with error listing remaining failures. Handles remap via `rewrite_create_table_ddl()`. Checks `table_exists()` before creation to count already-existing objects as progress.
- `create_functions()`: Phase 4 function creation. Iterates `manifest.functions` DDL entries, executes each via `ch.execute_ddl()`. Failures are logged as warnings and skipped (function may already exist). Logs creation count summary.

### Part Attachment (attach.rs)
- Uses `AttachParams` struct to bundle all parameters for a table's attachment
- Hardlinks files from `{backup_dir}/shadow/{db}/{table}/{part_name}/` to `{table_data_path}/detached/{part_name}/`
- Falls back to file copy on EXDEV (cross-device error code 18)
- Chowns to ClickHouse uid/gid detected from `stat()` on data_path; skips chown if not root
- `ALTER TABLE ATTACH PART` errors 232/233 (overlapping range, already exists) are logged as warnings and skipped

### UUID-Isolated S3 Restore (attach.rs, Phase 2c)
- S3 disk parts are restored via CopyObject instead of hardlink
- **UUID path derivation**: `store/{uuid_hex[0..3]}/{uuid_with_dashes}/{relative_path}` -- matches ClickHouse's internal S3 path convention
- `uuid_s3_prefix(uuid) -> String` generates the prefix from the destination table's UUID
- **Same-name optimization**: Before copying, calls `ListObjectsV2(prefix=store/{uuid_hex[0..3]}/{uuid_with_dashes}/)` to get existing S3 objects. Objects matching both path and size are skipped (zero-copy). Single ListObjectsV2 per table, not per-object HeadObject.
- S3 objects are copied from the backup bucket to the data bucket using `s3.copy_object_with_retry()`
- Parallel S3 copies within a table bounded by `object_disk_server_side_copy_concurrency` semaphore (default 32)
- After CopyObject: metadata files are rewritten using `object_disk::rewrite_metadata()` to update paths and set RefCount=0, ReadOnly=false
- Rewritten metadata files are written to `detached/{part_name}/` before ATTACH
- InlineData (v4+): no CopyObject needed for inline objects; preserved during rewrite
- `OwnedAttachParams` extended with S3-related fields: `s3_client`, `disk_type_map`, `object_disk_server_side_copy_concurrency`, `allow_object_disk_streaming`, `disk_remote_paths`

### Remap Integration in Restore Flow (Phase 4a)
When remap is active (`rename_as` or `database_mapping` provided):
- `RemapConfig` is built from CLI params inside `restore()` and threaded through to `create_databases()` and `create_tables()`
- Manifest table keys (`"db.table"`) are remapped to destination db/table via `remap_table_key()` for schema creation, data path resolution, UUID lookup, and `OwnedAttachParams` construction
- Resume state uses *original* manifest table keys (since state file and manifest parts reference original names), but queries `system.parts` using *destination* db/table names
- `find_table_data_path()` and `find_table_uuid()` use destination db/table since those are the live ClickHouse tables

### Resume State Tracking (Phase 2d)
- When `resume=true` (gated by both `--resume` CLI flag AND `config.general.use_resumable_state`):
  - Loads `RestoreState` from `{backup_dir}/restore.state.json` at start
  - For each table in manifest, queries `ch.query_system_parts(db, table)` to get currently active parts in ClickHouse
  - Merges state file info with live system.parts data (system.parts is authoritative)
  - The already-attached set is the union of (state file parts) and (system.parts active parts)
  - Parts whose name is in the already-attached set are skipped during ATTACH
  - After each successful ATTACH PART: adds to state, calls `save_state_graceful()` (non-fatal on failure per design 16.1)
  - On successful completion: `restore.state.json` is deleted
  - If `query_system_parts` fails (e.g., ClickHouse down): logs warning, falls back to state file only
- `RestoreState.attached_parts` is keyed by `"db.table"` -> `Vec<part_name>`
- `OwnedAttachParams` extended with `already_attached: HashSet<String>` and `restore_state_path: Option<PathBuf>` fields

### Detached Path Resolution
Queries `system.tables` for `data_paths` column to find the table's data directory, then appends `detached/{part_name}/`.

### Public API
- `restore(config, ch, backup_name, table_pattern, schema_only, data_only, resume, rename_as: Option<&str>, database_mapping: Option<&HashMap<String, String>>) -> Result<()>` -- Main entry point with resume support (Phase 2d) and remap support (Phase 4a); 9 parameters
- `create_databases(ch, manifest, remap: Option<&RemapConfig>) -> Result<()>` -- DDL for databases (remap-aware)
- `create_tables(ch, manifest, filter, data_only, remap: Option<&RemapConfig>) -> Result<()>` -- DDL for tables (remap-aware)
- `create_ddl_objects(ch, manifest, ddl_keys, remap: Option<&RemapConfig>) -> Result<()>` -- Phase 3: DDL-only objects with retry loop (remap-aware)
- `create_functions(ch, manifest) -> Result<()>` -- Phase 4: functions from manifest.functions DDL
- `classify_restore_tables(manifest, table_keys) -> RestorePhases` -- Split tables into data/postponed/DDL-only phases using streaming engine and refreshable MV detection
- `is_streaming_engine(engine) -> bool` -- Returns true for Kafka, NATS, RabbitMQ, S3Queue (Phase 4c)
- `is_refreshable_mv(tm: &TableManifest) -> bool` -- Returns true for MaterializedView with REFRESH clause in DDL (Phase 4c)
- `topological_sort(tables, keys) -> Result<Vec<String>>` -- Kahn's algorithm with engine-priority fallback and cycle detection
- `data_table_priority(table_key) -> u8` -- Priority for Phase 2 ordering (0=regular, 1=.inner)
- `engine_restore_priority(engine) -> u8` -- Priority for Phase 3 tie-breaking (0=Dictionary, 1=View/MV, 2=Distributed/Merge, 3=other)
- `RestorePhases` -- Classification result struct: data_tables, ddl_only_tables, postponed_tables
- `RemapConfig::new(rename_as_str, table_pattern, db_mapping_str, default_replica_path) -> Result<Option<Self>>` -- Build remap config from CLI flags (returns None when no remap active)
- `RemapConfig::remap_table_key(original_key) -> (String, String)` -- Map manifest key to destination db/table
- `parse_database_mapping(s) -> Result<HashMap<String, String>>` -- Parse `-m` CLI value
- `rewrite_create_table_ddl(ddl, src_db, src_table, dst_db, dst_table, default_replica_path) -> String` -- Full DDL rewrite
- `rewrite_create_database_ddl(ddl, src_db, dst_db) -> String` -- Database DDL rewrite
- `attach_parts(params) -> Result<u64>` -- Hardlink + ATTACH PART (borrowed params), returns count
- `attach_parts_owned(params) -> Result<u64>` -- Hardlink + ATTACH PART (owned params for tokio::spawn); handles both local and S3 disk parts; skips already-attached parts when resume is active (Phase 2d)
- `OwnedAttachParams` -- Owned variant of AttachParams with engine field for spawn boundaries; includes `s3_client`, `disk_type_map`, `disk_remote_paths`, `object_disk_server_side_copy_concurrency`, `allow_object_disk_streaming` for Phase 2c; `already_attached`, `restore_state_path` for Phase 2d
- `uuid_s3_prefix(uuid) -> String` -- Generate `store/{3char}/{uuid_with_dashes}/` prefix for S3 restore paths
- `detect_clickhouse_ownership(data_path) -> Result<(Option<u32>, Option<u32>)>` -- UID/GID detection
- `get_table_data_path(ch, db, table) -> Result<PathBuf>` -- Query data_paths
- `sort_parts_by_min_block(parts) -> Vec<PartInfo>` -- Sorted copy
- `needs_sequential_attach(engine) -> bool` -- Engine classification

### Parallel Restore Pattern (Phase 2a)
- Tables are restored in parallel, bounded by `effective_max_connections(config)` via a `tokio::Semaphore`
- Each `tokio::spawn` task: acquires permit -> `attach_parts_owned(OwnedAttachParams)` -> returns `(table_key, attached_count)`
- `OwnedAttachParams` uses owned types (`String`, `PathBuf`, `Vec<PartInfo>`) to cross `tokio::spawn` boundaries (no lifetime constraints)
- Engine-aware ATTACH routing: `needs_sequential_attach(engine)` returns true for Replacing/Collapsing/Versioned engines, ensuring sorted sequential ATTACH; plain MergeTree also uses sequential ATTACH within a single table (parallelism is across tables)
- `attach_parts_owned()` bridges to the internal `attach_parts_inner()` which accepts borrowed `AttachParams` -- no duplication of attach logic
- Uses `futures::future::try_join_all` for fail-fast error propagation

### Error Handling
- Uses `anyhow::Result` with `.context()` for error chain
- ATTACH PART errors for overlapping/existing parts are warnings, not failures
- Chown EPERM (not running as root) is silently skipped

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
