# CLAUDE.md -- src/restore

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the `restore` command -- reads a backup manifest, creates databases/tables from DDL, hardlinks data parts to ClickHouse's `detached/` directory, and executes `ALTER TABLE ATTACH PART`.

Supports Mode B (non-destructive, default) and Mode A (destructive `--rm` DROP, Phase 4d).

## Directory Structure

```
src/restore/
  mod.rs      -- Entry point: restore() orchestrates phased restore flow, ATTACH TABLE mode, mutation re-apply
  topo.rs     -- Table classification, topological sort, engine priority, DROP ordering (Phase 4d), streaming engine detection (Phase 4b+4c)
  remap.rs    -- Table/database remap, DDL rewriting, ON CLUSTER injection, ZK param parsing, Distributed cluster rewrite
  schema.rs   -- CREATE/DROP DATABASE/TABLE, ZK conflict resolution, DatabaseReplicated detection, create_ddl_objects, create_functions
  attach.rs   -- Hardlink parts to detached/, chown, ATTACH PART
  rbac.rs     -- RBAC restore, config restore, named collections restore, restart_command execution (Phase 4e)
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

### DDL Helpers for ON CLUSTER, ZK, Distributed Cluster Rewrite (remap.rs, Phase 4d)
All Phase 4d remap functions follow the same pure (no async, no I/O) pattern as existing remap functions.

- **`parse_replicated_params(ddl)`**: Parses ZK path and replica name from a `Replicated*MergeTree` DDL. Finds the `Replicated` engine marker, then extracts the first and second single-quoted arguments from the engine parameters. Returns `None` for non-Replicated engines or short syntax (empty parens). Returns `Option<(String, String)>` of `(zk_path, replica_name)`.
- **`resolve_zk_macros(template, macros)`**: Substitutes `{key}` patterns in a ZK path template from a `HashMap<String, String>`. Common keys: `{database}`, `{table}`, `{shard}`, `{replica}`, `{uuid}`. Unknown macros are left as-is.
- **`add_on_cluster_clause(ddl, cluster)`**: Injects `ON CLUSTER '{cluster}'` into DDL statements. Handles CREATE TABLE/DATABASE/VIEW/DICTIONARY/MATERIALIZED VIEW and DROP TABLE/DATABASE, with or without IF [NOT] EXISTS. Uses `skip_object_name()` helper to find the insertion point after the object identifier. Returns DDL unchanged if ON CLUSTER already present.
- **`rewrite_distributed_cluster(ddl, new_cluster)`**: Rewrites the cluster name in a Distributed engine DDL. Finds `Distributed(` marker, locates the first single-quoted argument (cluster), and replaces its value. Returns DDL unchanged for non-Distributed engines.

### Phased Restore Architecture (Phase 4b+4c+4d, mod.rs)
The restore flow is structured into explicit phases per design doc 5.1/5.2/5.3/5.5/5.6/5.7:
```
Phase 0:   DROP tables/databases      (Mode A --rm only: drop_tables + drop_databases)
Phase 1:   CREATE databases           (create_databases, with ON CLUSTER)
Phase 2:   CREATE + ATTACH data tables (sorted by engine priority, with ZK conflict resolution, ON CLUSTER, Distributed cluster rewrite)
Phase 2.5: Re-apply pending mutations (reapply_pending_mutations, after all data attached)
Phase 2b:  CREATE postponed tables    (streaming engines, refreshable MVs -- Phase 4c)
Phase 3:   CREATE DDL-only objects    (topologically sorted, with ON CLUSTER)
Phase 4:   CREATE functions           (from manifest.functions, with ON CLUSTER)
Phase 4b:  Restore named collections (from manifest.named_collections DDL, with ON CLUSTER)
Phase 4c:  Restore RBAC              (DDL-based from access/*.jsonl, with rbac_resolve_conflicts)
Phase 4d:  Restore config files      (file copy from configs/ to config_dir)
Phase 4e:  Execute restart commands   (exec:/sql: prefixed commands, semicolon-separated)
```
Before Phase 0, `projection_gate_error()` refuses the restore outright when the manifest's `stripped_projections` is non-empty and the server is older than `MIN_PROJECTION_TOLERANT_VERSION` (24.3). Those parts' `checksums.txt` still names the stripped `.proj` entries, and a pre-24.3 server runs a consistency pass that rejects the whole part with `Code: 107 FILE_DOESNT_EXIST`. Gated by `backup.strict_projection_gate` (default true). An empty `stripped_projections` -- which includes every manifest written before the field existed -- and an unparseable version string both proceed, so backups already in S3 restore exactly as before.

- `classify_restore_tables()` splits filtered table keys into `RestorePhases` (data_tables, postponed_tables, ddl_only_tables)
- Phase 0 (Mode A) runs `drop_tables()` (reverse engine priority with retry loop) then `drop_databases()`, gated by `rm && !data_only`
- Phase 2 passes `phases.data_tables` to `create_tables()` and the data attach loop (instead of all table_keys)
- Phase 2 includes optional ATTACH TABLE mode for Replicated engines when `config.clickhouse.restore_as_attach` is true
- Phase 2.5 runs `reapply_pending_mutations()` for tables with `pending_mutations` in the manifest (design 5.7)
- Phase 2b creates postponed tables (streaming engines + refreshable MVs) AFTER all data is attached but BEFORE DDL-only objects. This prevents streaming engines from consuming data prematurely during restore.
- Phase 3 runs `topological_sort()` on `phases.ddl_only_tables`, then `create_ddl_objects()` on the sorted result
- Phase 4 calls `create_functions()` for manifest.functions DDL
- Schema-only mode (`--schema-only`) creates all schema (Phases 0-4) but skips data attach. Phase 2b runs AFTER Phase 3 DDL-only objects (since DDL-only objects like regular MVs may be targets that streaming engines write to).
- Data-only mode (`--data-only`) skips Phases 0, 1, 2b, 3, and 4
- Resume state only queries `system.parts` for data tables (DDL-only and postponed objects have no parts)

### Cross-Cutting Features (Phase 4d, mod.rs + schema.rs)
These features are wired into the restore orchestrator and affect multiple phases:
- **ON CLUSTER DDL**: When `config.clickhouse.restore_schema_on_cluster` is non-empty, all CREATE/DROP DDL statements get `ON CLUSTER '{cluster}'` appended via `add_on_cluster_clause()`. Skipped for databases in `replicated_databases` set (DatabaseReplicated handles its own replication).
- **DatabaseReplicated detection**: `detect_replicated_databases()` queries `ch.query_database_engine()` for each unique database in the manifest. Databases with engine == "Replicated" are added to a `HashSet<String>` that gates ON CLUSTER skipping.
- **ZK conflict resolution**: Before creating Replicated tables in `create_tables()`, `resolve_zk_conflict()` parses ZK path + replica from DDL, resolves macros, and calls `ch.check_zk_replica_exists()`, which returns the tri-state `ZkReplicaCheck` (`Exists` / `Absent` / `Indeterminate`). `zk_check_action(check, strict)` maps it to a `ZkAction`; see "ZK Check Tri-State" below. A failed `SYSTEM DROP REPLICA` remains non-fatal.
- **`{uuid}` macro in ZK paths**: When `clickhouse.replace_uuid_macro` is true (default **false**), `replace_uuid_macro(ddl, table_uuid)` substitutes the manifest's recorded table UUID for a literal `{uuid}` inside the Replicated engine's ZooKeeper path before CREATE. ClickHouse expands `{uuid}` only for an `ON CLUSTER` CREATE, inside a DatabaseReplicated database, for an ATTACH, or with an explicit UUID clause; a plain per-host CREATE otherwise fails with BAD_ARGUMENTS (error 36), so this is an opt-in fix for that case rather than a guard against silent divergence. The substitution is confined to the byte span found by `replicated_zk_path_span()`, so a `{uuid}` in a column DEFAULT, a comment, or `SETTINGS` is left for ClickHouse to interpret. No-op when the manifest has no UUID, the engine is not Replicated, or the path has no `{uuid}`.
- **Replica sync strictness**: `check_replicas_before_attach` polls `system.replicas` per Replicated table; `decide_replica_sync(&synced, strict)` turns the outcome into `Attach` or `Skip`. `Ok(false)` -- polling completed and the replica is still behind -- skips the table under the default `clickhouse.strict_replica_sync = true`, and all of its parts are added to `unsynced_parts`, which feeds `total_skipped` and therefore exit 3. `Err` is *indeterminate* (the check failed, which is not evidence of lag) and always warns and proceeds, in both modes. The hazard being avoided is a partial restore visible to readers, not row duplication: ATTACH PART deduplicates on a content-derived key, so re-attaching an identical part does not duplicate rows.
- **Distributed cluster rewrite**: When `config.clickhouse.restore_distributed_cluster` is non-empty, `rewrite_distributed_cluster()` is applied during `create_tables()` to change the cluster name in Distributed engine DDL.
- **Macros resolution**: `ch.get_macros()` is called once at restore start; result is passed to ZK resolution and ATTACH TABLE mode. Default `{database}` and `{table}` entries are added per-table from the destination names.

### Table Classification and Topological Sort (topo.rs)
- **`RestorePhases`** struct: `data_tables: Vec<String>`, `ddl_only_tables: Vec<String>`, `postponed_tables: Vec<String>`
- **`classify_restore_tables(manifest, table_keys)`**: Splits tables using a priority decision tree: (1) streaming engine -> postponed, (2) refreshable MV -> postponed, (3) metadata_only -> ddl_only, (4) else -> data. Data tables sorted by `data_table_priority()` (regular=0, `.inner` tables=1). Logs classification counts including postponed.
- **`is_streaming_engine(engine)`**: Returns true for Kafka, NATS, RabbitMQ, S3Queue. These engines consume data from external sources and must not be created until data tables are fully attached.
- **`is_refreshable_mv(tm)`**: Returns true if `tm.engine == "MaterializedView"` AND DDL contains the `REFRESH` keyword (case-insensitive, preceded by whitespace or newline). Refreshable MVs run scheduled queries that should not execute against incomplete data.
- **`data_table_priority(table_key)`**: Returns 0 for regular tables, 1 for `.inner`/`.inner_id` tables (MV storage targets created before MVs)
- **`engine_restore_priority(engine)`**: Per design doc 5.1: Dictionary=0, View/MaterializedView/LiveView/WindowView=1, Distributed/Merge=2, other=3
- **`engine_drop_priority(engine)`**: Reverse of `engine_restore_priority()` for Mode A DROP ordering (Phase 4d): Distributed/Merge=0 (dropped first), View/MV/LiveView/WindowView=1, Dictionary=2, regular data tables=3 (dropped last)
- **`sort_tables_for_drop(manifest, table_keys)`**: Sorts ALL table keys by `engine_drop_priority` for Mode A. Takes all tables (not just DDL-only) and produces a single sorted list. Used by `drop_tables()` in schema.rs.
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

### Schema Creation and DROP (schema.rs)
- `create_databases()`: Executes DDL from `manifest.databases[]` with `IF NOT EXISTS` safety. When remap is active, creates target databases with rewritten DDL; tracks created databases to avoid duplicates. Accepts `on_cluster` and `replicated_databases` params (Phase 4d) for ON CLUSTER injection.
- `create_tables()`: For each filtered table, checks existence first, then executes the stored `CREATE TABLE` DDL. When remap is active, rewrites DDL before execution and checks existence using destination db/table names. Phase 4d additions: runs `resolve_zk_conflict()` for Replicated tables before CREATE, applies `rewrite_distributed_cluster()` when `dist_cluster` is set, injects ON CLUSTER clause. Accepts `on_cluster`, `replicated_databases`, `macros`, and `dist_cluster` params.
- `create_ddl_objects()`: Phase 3 DDL-only object creation with retry-loop fallback. Creates objects sequentially in caller-provided (topologically sorted) order. On failure, queues for retry (max 10 rounds). Each round retries failed objects; if zero progress in a round after round 0, bails with error listing remaining failures. Handles remap via `rewrite_create_table_ddl()`. Checks `table_exists()` before creation to count already-existing objects as progress. Accepts `on_cluster` and `replicated_databases` params (Phase 4d).
- `create_functions()`: Phase 4 function creation. Iterates `manifest.functions` DDL entries, executes each via `ch.execute_ddl()`. Failures are logged as warnings and skipped (function may already exist). Logs creation count summary. Accepts `on_cluster` param (Phase 4d) for ON CLUSTER injection.
- `drop_tables()` (Phase 4d): Mode A DROP phase. Sorts tables by `engine_drop_priority` (Distributed/Merge first, data tables last). Uses retry loop (max 10 rounds, same pattern as `create_ddl_objects()`). System databases are never dropped. Respects remap (drops destination names) and ON CLUSTER (skipped for DatabaseReplicated databases).
- `drop_databases()` (Phase 4d): Mode A database DROP. Iterates manifest databases, skips system databases (`system`, `information_schema`, `INFORMATION_SCHEMA`). Uses `ch.drop_database()` with ON CLUSTER support.
- `resolve_zk_conflict(ch, ddl, macros, table_uuid, table, strict)`: ZK conflict resolution for Replicated tables. Flow: `parse_replicated_params()` -> `resolve_zk_macros()` -> `ch.check_zk_replica_exists()` -> `zk_check_action()` -> `ch.drop_replica_from_zkpath()` when a conflict is found. Returns `Err` only for `ZkAction::FailTable`, which the caller must let block that table's CREATE.
- `detect_replicated_databases()` (Phase 4d): Queries `ch.query_database_engine()` for each unique database in the manifest. Returns `HashSet<String>` of databases using the "Replicated" engine (DatabaseReplicated).
- `is_replicated_engine()` (Phase 4d): Returns true if engine name starts with "Replicated" (covers all Replicated*MergeTree variants). Used by ZK conflict resolution, ATTACH TABLE mode, and schema creation logic.
- `SYSTEM_DATABASES` (Phase 4d): Constant `&[&str]` listing databases that must never be dropped: `["system", "information_schema", "INFORMATION_SCHEMA"]`.

### ZK Check Tri-State (schema.rs + clickhouse/client.rs)
`check_zk_replica_exists()` returns `ZkReplicaCheck`, not a bool. It **does not fail open**:

| `ZkReplicaCheck` | Meaning | `zk_check_action` (strict) | (non-strict) |
|------------------|---------|----------------------------|--------------|
| `Exists` | A replica is registered at the path | `DropReplica` | `DropReplica` |
| `Absent` | ZooKeeper was queried and holds no such replica | `Proceed` | `Proceed` |
| `Indeterminate` | The query failed; ZK state is unknown | `FailTable` | `Proceed` (with a warning) |

`Indeterminate` is the load-bearing cell. "ZooKeeper says there is no replica" and "we could not ask ZooKeeper" justify opposite decisions; folding the latter into `Absent` would send an unreachable ZooKeeper down the same clean path as a verified-clean one, and the stale replica this check exists to drop would go undropped. Under the default `clickhouse.zk_check_strict = true` it means `FailTable`, and `resolve_zk_conflict()` returns an `Err` naming the table, the replica, and the ZK path. Setting `zk_check_strict = false` restores the old create-anyway behaviour, logging that conflict resolution was skipped and that whether a stale replica exists is unknown.

### ATTACH TABLE Completeness Pre-Check (mod.rs)
ATTACH TABLE mode hardlinks a table's parts into its live data directory, so a table brought up with parts missing is silently short of data. The pre-flight resolves every part's staged source, and `decide_attach_table(resolved, require_complete)` returns one of:

- `Attach(CompleteAttachPlan)` -- every part resolved.
- `Refuse { missing, total }` -- parts are missing and `clickhouse.attach_table_require_complete` is true (the default). The table is **neither created nor attached**, and all of its parts are added to `AttachPreflight::refused_parts`, which feeds `total_skipped` and the exit-3 partial-restore error.
- `AttachIncomplete { plan, missing, total }` -- parts are missing but the operator set `attach_table_require_complete = false`.

`CompleteAttachPlan` lives in the private `mod attach_plan` with a module-private `sources` field, so the rest of `restore` cannot forge one with a struct literal; its only safe constructor, `try_new`, fails unless every part resolved. `try_attach_table_mode()` takes the plan **by value**, so holding one is proof it came from `decide_attach_table` — the incomplete case is reachable only through the `attach_available_anyway` escape hatch, which is private to that module. No DETACH TABLE, hardlink, ATTACH TABLE, or SYSTEM RESTORE REPLICA can run for a table with unresolved parts unless the operator opted out.

### Path Encoding (attach.rs, mod.rs)
- `url_encode()` in attach.rs has been removed; all call sites (including those in mod.rs that previously referenced `attach::url_encode`) now use `crate::path_encoding::encode_path_component()`

### Part Attachment (attach.rs)
- Uses `AttachParams` struct to bundle all parameters for a table's attachment
- `OwnedAttachParams` extended with per-disk fields:
  - `manifest_disks: HashMap<String, String>` -- disk name -> disk path from manifest for per-disk shadow path resolution
  - `source_db: String` -- original (pre-remap) database name for shadow path lookup
  - `source_table: String` -- original (pre-remap) table name for shadow path lookup
- `attach_parts_inner()` uses `resolve_shadow_part_path()` with `source_db`/`source_table` to resolve per-disk shadow paths. Builds `part_to_disk` reverse map from `parts_by_disk` to look up each part's disk name.
- This fixes a pre-existing inconsistency: previously `attach_parts_inner()` used destination db/table names for shadow lookup, which broke under remap (`--as`). Now both `attach_parts_inner()` and `try_attach_table_mode()` consistently use source names.
- Falls back to file copy on EXDEV (cross-device error code 18)
- Chowns to ClickHouse uid/gid detected from `stat()` on data_path; skips chown if not root

### ATTACH PART Error Classification (attach.rs)
Both ATTACH call sites route their error string through the single `classify_attach_error()`, which returns an `AttachErrorClass`. The codes are traced to ClickHouse's `ErrorCodes.cpp` and are identical across the four CI matrix versions:

- `MissingPart` -- 232 `NO_SUCH_DATA_PART` or 233 `BAD_DATA_PART_NAME`. The data is genuinely absent.
- `DuplicatePart` -- 235 `DUPLICATE_DATA_PART`. An identically named part is already active.
- `TemporarilyLocked` -- 384 `PART_IS_TEMPORARILY_LOCKED`. The part exists but is being removed.
- `Unknown` -- anything else. **Fatal.** Never treat an unrecognised ATTACH failure as benign.

**Missing-part is tested FIRST, and that ordering is part of the contract.** Only `MissingPart` sets `counts_as_missing_data()`, and only those parts feed `AttachResult.skipped`, which the caller turns into `ChBackupError::PartialRestore` and CLI exit 3. `DuplicatePart` and `TemporarilyLocked` are logged per part and tallied separately, so an idempotent re-restore over already-present parts still exits 0. Testing duplicate before missing is what previously let a missing part be reported as "already exists", omitted from the tally, and a failed restore exit 0.

### UUID-Isolated S3 Restore (attach.rs, Phase 2c)
- S3 disk parts are restored via CopyObject instead of hardlink
- **UUID path derivation**: `store/{uuid_hex[0..3]}/{uuid_with_dashes}/{relative_path}` -- matches ClickHouse's internal S3 path convention
- `uuid_s3_prefix(uuid) -> String` generates the prefix from the destination table's UUID
- **Same-name optimization**: Before copying, calls `ListObjectsV2(prefix=store/{uuid_hex[0..3]}/{uuid_with_dashes}/)` to get existing S3 objects. Objects matching both path and size are skipped (zero-copy). Single ListObjectsV2 per table, not per-object HeadObject.
- S3 objects are copied from the backup bucket to the data bucket using `s3.copy_object_with_retry()`
- Parallel S3 copies within a table bounded by `object_disk_server_side_copy_concurrency` semaphore (default 32)
- After CopyObject: metadata files are rewritten using `object_disk::rewrite_metadata()` to update paths and set RefCount=0, ReadOnly=false. Keys are re-derived per metadata version by `object_disk::restore_object_keys()`: a v5 file gets the complete destination key (ClickHouse resolves it with `createAsAbsolute`), a v2-v4 file keeps the bare disk-relative form. Before that, `to_disk_relative_keys()` maps each v5 key in the staged file back to the disk-relative key the manifest recorded (matching on a path-component boundary, disambiguated by size) and **fails the part** if the match is missing or ambiguous, rather than writing a key that CopyObject never created
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

### ATTACH TABLE Mode (Phase 4d, mod.rs)
When `config.clickhouse.restore_as_attach` is true, Replicated*MergeTree tables use an alternative restore flow instead of per-part ATTACH:
1. `ch.detach_table_sync(db, table)` -- detach the existing (freshly created) table
2. Parse ZK path + replica from DDL via `parse_replicated_params()`, resolve macros
3. `ch.drop_replica_from_zkpath(replica, path)` -- clean ZK state (non-fatal on failure)
4. Hardlink/copy parts to the table's **main data directory** (NOT `detached/`) via `spawn_blocking` + `hardlink_or_copy_dir()` + `chown_recursive()`
5. `ch.attach_table(db, table)` -- re-attach the table (reads from data directory)
6. `ch.system_restore_replica(db, table)` -- rebuild replica metadata from local parts

Key behaviors:
- Eligibility is decided by the completeness pre-check above; a refused table never reaches step 1
- Non-Replicated tables always use the normal per-part ATTACH flow
- If any step fails, falls back to normal per-part ATTACH with a warning log
- `try_attach_table_mode()` returns `Ok(true)` on success, `Ok(false)` if not eligible, `Err` on failure
- Results from ATTACH TABLE mode are merged with per-part ATTACH results for the final tally
- The `is_replicated_engine()` helper (in schema.rs) checks `engine.starts_with("Replicated")`
- Per-disk path resolution: accepts `manifest_disks` and `parts_by_disk` parameters, builds `part_to_disk` reverse map, and uses `resolve_shadow_part_path()` per-part instead of a single `shadow_base` directory. Uses `src_db`/`src_table` (source names) for shadow lookup, consistent with `attach_parts_inner()`.

### Mutation Re-Apply (Phase 4d, mod.rs)
After all data parts are attached (Phase 2) and before Phase 2b, `reapply_pending_mutations()` iterates restored tables and checks each `TableManifest.pending_mutations`. For each non-empty list:
- Logs a warning per table with mutation count (design 5.7)
- For each `MutationInfo`: logs mutation_id + command + parts_pending, then calls `ch.execute_mutation(db, table, command)` with `mutations_sync=2` (waits for completion)
- Failures are logged as warnings but do NOT abort restore (partial mutation re-apply is better than no data)
- Mutations are applied sequentially per-table (order matters for correctness)
- When remap is active, uses destination db/table for the ALTER TABLE statement
- Skipped when `schema_only == true`

### RBAC, Config, Named Collections Restore and Restart Command (rbac.rs, Phase 4e)
Phase 4e adds four restore extensions wired into the restore orchestrator after `create_functions()` (Phase 4), in both the normal and schema-only restore paths:

- **`restore_named_collections(ch, manifest, on_cluster)`**: Follows `create_functions()` pattern. Iterates `manifest.named_collections` DDL entries, executes each via `ch.execute_ddl()`. Supports ON CLUSTER injection via `add_on_cluster_clause()`. Failures logged as warnings (non-fatal).
- **`restore_rbac(ch, config, backup_dir, resolve_conflicts)`**: DDL-based RBAC restore from `.jsonl` files in `{backup_dir}/access/`. Parses `RbacEntry` structs (entity_type, name, create_statement) from JSONL. Conflict resolution via `rbac_resolve_conflicts` config:
  - `"recreate"` (default): `DROP IF EXISTS` then `CREATE` for each entity
  - `"ignore"`: Skip on error (object already exists)
  - `"fail"`: Return error on any failure
  - After restore: removes stale `.list` files from ClickHouse's `{data_path}/access/`, creates `need_rebuild_lists.mark`, chowns to ClickHouse uid/gid (uses `detect_clickhouse_ownership()` from attach.rs)
- **`restore_configs(config, backup_dir)`**: Copies files from `{backup_dir}/configs/` to `config.clickhouse.config_dir` using `spawn_blocking` + `walkdir`, preserving directory structure.
- **`execute_restart_commands(ch, restart_command)`**: Semicolon-separated commands with prefix routing:
  - `exec:` prefix: Execute as shell command via `sh -c` (default if no prefix)
  - `sql:` prefix: Execute as ClickHouse DDL via `ch.execute_ddl()`
  - All failures are non-fatal (logged as warnings per design 5.6)
- **`make_drop_ddl(entity_type, name)`**: Generates `DROP {entity} IF EXISTS` DDL for RBAC entities. Backtick-quotes names. Returns `None` for unknown entity types.
- **`chown_recursive(dir, uid, gid)`**: Recursively chowns a directory using `nix::unistd::chown` via `walkdir`.
- Phase 4e extensions are gated by CLI flags OR `*_backup_always` config values (same OR logic as backup side)
- `restart_command` execution is triggered only when RBAC or config restore actually occurred (checked via `access/` or `configs/` directory existence)

### Detached Path Resolution
Queries `system.tables` for `data_paths` column to find the table's data directory, then appends `detached/{part_name}/`.

### Public API
- `restore(config, ch, backup_name, table_pattern, schema_only, data_only, rm, resume, rename_as: Option<&str>, database_mapping: Option<&HashMap<String, String>>, rbac: bool, configs: bool, named_collections: bool) -> Result<()>` -- Main entry point with Mode A/B support (Phase 4d), resume (Phase 2d), remap (Phase 4a), and RBAC/config/named-collections restore (Phase 4e); 13 parameters
- `create_databases(ch, manifest, remap, on_cluster, replicated_databases) -> Result<()>` -- DDL for databases (remap-aware, ON CLUSTER)
- `create_tables(ch, manifest, filter, data_only, remap, on_cluster, replicated_databases, macros, dist_cluster) -> Result<()>` -- DDL for tables (remap-aware, ON CLUSTER, ZK conflict resolution, Distributed cluster rewrite)
- `create_ddl_objects(ch, manifest, ddl_keys, remap, on_cluster, replicated_databases) -> Result<()>` -- Phase 3: DDL-only objects with retry loop (remap-aware, ON CLUSTER)
- `create_functions(ch, manifest, on_cluster) -> Result<()>` -- Phase 4: functions from manifest.functions DDL (ON CLUSTER)
- `drop_tables(ch, manifest, table_keys, remap, on_cluster, replicated_databases) -> Result<()>` -- Phase 0: Mode A DROP with reverse engine priority and retry loop (Phase 4d)
- `drop_databases(ch, manifest, remap, on_cluster, replicated_databases) -> Result<()>` -- Phase 0: Mode A database DROP (Phase 4d)
- `detect_replicated_databases(ch, manifest) -> HashSet<String>` -- Query database engines to find DatabaseReplicated databases (Phase 4d)
- `is_replicated_engine(engine) -> bool` -- Returns true for Replicated*MergeTree engines (Phase 4d)
- `classify_restore_tables(manifest, table_keys) -> RestorePhases` -- Split tables into data/postponed/DDL-only phases using streaming engine and refreshable MV detection
- `is_streaming_engine(engine) -> bool` -- Returns true for Kafka, NATS, RabbitMQ, S3Queue (Phase 4c)
- `is_refreshable_mv(tm: &TableManifest) -> bool` -- Returns true for MaterializedView with REFRESH clause in DDL (Phase 4c)
- `topological_sort(tables, keys) -> Result<Vec<String>>` -- Kahn's algorithm with engine-priority fallback and cycle detection
- `data_table_priority(table_key) -> u8` -- Priority for Phase 2 ordering (0=regular, 1=.inner)
- `engine_restore_priority(engine) -> u8` -- Priority for Phase 3 tie-breaking (0=Dictionary, 1=View/MV, 2=Distributed/Merge, 3=other)
- `engine_drop_priority(engine) -> u8` -- Reverse priority for Phase 0 DROP ordering (0=Distributed/Merge, 1=Views, 2=Dictionary, 3=data tables) (Phase 4d)
- `sort_tables_for_drop(manifest, table_keys) -> Vec<String>` -- Sort all tables by engine_drop_priority for Mode A (Phase 4d)
- `RestorePhases` -- Classification result struct: data_tables, ddl_only_tables, postponed_tables
- `RemapConfig::new(rename_as_str, table_pattern, db_mapping_str, default_replica_path) -> Result<Option<Self>>` -- Build remap config from CLI flags (returns None when no remap active)
- `RemapConfig::remap_table_key(original_key) -> (String, String)` -- Map manifest key to destination db/table
- `parse_database_mapping(s) -> Result<HashMap<String, String>>` -- Parse `-m` CLI value
- `rewrite_create_table_ddl(ddl, src_db, src_table, dst_db, dst_table, default_replica_path) -> String` -- Full DDL rewrite
- `rewrite_create_database_ddl(ddl, src_db, dst_db) -> String` -- Database DDL rewrite
- `parse_replicated_params(ddl) -> Option<(String, String)>` -- Extract ZK path and replica from Replicated engine DDL (Phase 4d)
- `resolve_zk_macros(template, macros) -> String` -- Substitute `{key}` macros in ZK path template (Phase 4d)
- `add_on_cluster_clause(ddl, cluster) -> String` -- Inject ON CLUSTER into DDL statements (Phase 4d)
- `rewrite_distributed_cluster(ddl, new_cluster) -> String` -- Rewrite cluster name in Distributed engine DDL (Phase 4d)
- `restore_named_collections(ch, manifest, on_cluster) -> Result<()>` -- Restore named collections from manifest DDL with ON CLUSTER support (Phase 4e)
- `restore_rbac(ch, config, backup_dir, resolve_conflicts) -> Result<()>` -- DDL-based RBAC restore from .jsonl files with conflict resolution (Phase 4e)
- `restore_configs(config, backup_dir) -> Result<()>` -- Copy config files from backup to ClickHouse config dir (Phase 4e)
- `execute_restart_commands(ch, restart_command) -> Result<()>` -- Execute semicolon-separated restart commands with exec:/sql: prefix routing (Phase 4e)
- `attach_parts_owned(params) -> Result<u64>` -- Hardlink + ATTACH PART (owned params for tokio::spawn); handles both local and S3 disk parts; skips already-attached parts when resume is active (Phase 2d)
- `OwnedAttachParams` -- Owned variant of AttachParams with engine field for spawn boundaries; includes `s3_client`, `disk_type_map`, `disk_remote_paths`, `object_disk_server_side_copy_concurrency`, `allow_object_disk_streaming` for Phase 2c; `already_attached`, `restore_state_path` for Phase 2d; `manifest_disks`, `source_db`, `source_table` for per-disk path resolution
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
- ATTACH PART errors are classified by `classify_attach_error()`; only already-present parts are warnings, missing parts count as skipped, and an unrecognised error is fatal
- Any skipped part makes `restore()` return `ChBackupError::PartialRestore` (CLI exit 3), and completion state is deliberately not persisted so a later `--resume` still retries those parts
- Chown EPERM (not running as root) is silently skipped

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
