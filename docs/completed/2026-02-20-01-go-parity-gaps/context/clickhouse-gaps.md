# ClickHouse Client Operations: Go vs Rust Parity Analysis

**Date:** 2026-02-20
**Scope:** `pkg/clickhouse/` (Go) vs `src/clickhouse/` + `src/backup/` + `src/restore/` (Rust)

---

## 1. Connection Setup

### Go Implementation
- **Protocol:** Native TCP protocol via `github.com/ClickHouse/clickhouse-go/v2` driver
- **Default port:** 9000 (native protocol)
- **Connection retry:** Infinite loop with 5-second intervals, `BreakConnectOnError` flag to abort
- **Connection pooling:** `MaxOpenConns` = `config.MaxConnections`, `ConnMaxLifetime` = 0 (intentional for RBAC rebuild), `MaxIdleConns` = 0
- **Timeout settings injected into connection:**
  - `connect_timeout`: from `config.Timeout` (default "30m") parsed and converted to integer seconds
  - `receive_timeout`: same as connect_timeout
  - `send_timeout`: same as connect_timeout
  - `http_send_timeout`: hardcoded 300 seconds
  - `http_receive_timeout`: hardcoded 300 seconds
- **Query logging:** `log_queries: 0 or 1` based on `config.LogSQLQueries`
- **TLS:** Uses `utils.NewTLSConfig(TLSCa, TLSCert, TLSKey, SkipVerify, true)` -- proper TLS config construction with CA, client certs, and skip-verify support
- **Ping:** After `clickhouse.Open()`, calls `Ping()` for validation
- **Thread safety:** `sync.Mutex` protects `conn` field

### Rust Implementation
- **Protocol:** HTTP protocol via `clickhouse-rs` crate (`clickhouse::Client`)
- **Default port:** 9000 (in config defaults, but this is the native port -- should be 8123 for HTTP)
- **Connection retry:** None -- single attempt, no retry
- **Connection pooling:** Delegated to `clickhouse-rs` internal HTTP client (hyper/reqwest)
- **Timeout settings:** Not configured -- relies on `clickhouse-rs` defaults
- **Query logging:** `log_sql_queries` flag controls Rust-side tracing level (info vs debug), but no server-side `log_queries` setting
- **TLS:** Sets `SSL_CERT_FILE` env var for CA cert; client certs via env vars warned as unsupported; `skip_verify` warned as unsupported
- **Ping:** `SELECT 1` query
- **Thread safety:** `Clone`-based (HTTP client is inherently safe to clone)

### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1.1 | **No connection timeout configuration** | HIGH | Go sets `connect_timeout`, `receive_timeout`, `send_timeout` as ClickHouse server-side settings on the connection. Rust sets none. Long-running queries (e.g., SYSTEM SYNC REPLICA on large tables) may hang indefinitely. |
| 1.2 | **No connection retry logic** | MEDIUM | Go has infinite retry with 5s intervals (important for startup when CH is still booting). Rust fails immediately on first connection error. |
| 1.3 | **No server-side query logging** | LOW | Go sets `log_queries: 0/1` on the ClickHouse connection. Rust only controls local tracing level. Not critical but useful for debugging. |
| 1.4 | **TLS client certificate not supported** | MEDIUM | Go properly constructs a TLS config with CA, client cert, and client key. Rust warns that client certs are not supported by the clickhouse-rs HTTP backend. |
| 1.5 | **skip_verify not functional** | MEDIUM | Go TLS config supports `InsecureSkipVerify`. Rust warns it is not supported. Self-signed certs in dev/test environments will fail. |
| 1.6 | **Default port mismatch** | LOW | Rust config defaults to port 9000 (native protocol) but uses HTTP protocol which needs 8123. Users must always override. The Go default of 9000 matches its native protocol usage. This may just be a config documentation issue rather than a real bug if users always specify the port. |

---

## 2. FREEZE / UNFREEZE Operations

### Go Implementation
- **Whole-table freeze:** `ALTER TABLE ... FREEZE [WITH NAME '...']` -- `WITH NAME` requires v19.1+
- **By-part freeze:** `FreezeTableByParts()` queries `system.parts` for distinct `partition_id`, then freezes each partition. Handles `partition_id = "all"` by using `FREEZE PARTITION tuple()` for unpartitioned tables.
- **freeze_by_part_where:** Appends extra WHERE clause to partition query
- **SYNC REPLICA before freeze:** When `SyncReplicatedTables=true` and engine starts with "Replicated", runs `SYSTEM SYNC REPLICA` before FREEZE. Failure is a warning (non-fatal).
- **Error handling:**
  - Code 60 (UNKNOWN_TABLE): handled when `IgnoreNotExistsErrorDuringFreeze=true`
  - Code 81 (UNKNOWN_DATABASE): handled when `IgnoreNotExistsErrorDuringFreeze=true`
  - Code 218 (CANNOT_FREEZE_PARTITION): handled when `IgnoreNotExistsErrorDuringFreeze=true`
- **UNFREEZE:** Uses `ALTER TABLE ... UNFREEZE WITH NAME`

### Rust Implementation
- **Whole-table freeze:** `ALTER TABLE ... FREEZE WITH NAME '...'` -- always uses WITH NAME
- **By-part freeze:** `freeze_partition()` with `ALTER TABLE ... FREEZE PARTITION '...' WITH NAME '...'`. `query_distinct_partitions()` gets partition IDs with optional `extra_where`.
- **freeze_by_part_where:** Supported via `query_distinct_partitions` extra_where param
- **SYNC REPLICA before freeze:** `sync_replicas()` in `backup/sync_replica.rs` filters for `engine.contains("Replicated")`, calls `SYSTEM SYNC REPLICA`. Failure is a warning.
- **Error handling:**
  - Code 60 (UNKNOWN_TABLE): Handled via string match `"UNKNOWN_TABLE"` or `"Code: 60"`
  - Code 81 (UNKNOWN_DATABASE): Handled via string match `"UNKNOWN_DATABASE"` or `"Code: 81"`
  - Code 218: Handled in `backup/mod.rs` freeze-by-part path
- **UNFREEZE:** Uses `ALTER TABLE ... UNFREEZE WITH NAME`

### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 2.1 | **No `tuple()` handling for unpartitioned tables** | MEDIUM | Go uses `FREEZE PARTITION tuple()` when `partition_id = "all"` (unpartitioned tables). Rust uses the raw partition_id string. If ClickHouse returns `"all"` as the partition_id for unpartitioned tables, `FREEZE PARTITION 'all'` may fail. Need to verify behavior. |

---

## 3. System Table Queries

### 3.1 system.tables (GetTables)

#### Go Implementation
- **Dynamic column selection:** `IsSystemTablesFieldPresent` checks which columns exist (data_path, data_paths, uuid, create_table_query, total_bytes) by querying `system.columns`
- **Excluded databases:** MySQL, PostgreSQL, MaterializedPostgreSQL (engine-based exclusion, not database name)
- **Excluded by pattern:** `skip_tables` patterns via `filepath.Match`
- **Excluded engines:** `skip_table_engines` list (case-insensitive)
- **is_temporary filter:** `WHERE is_temporary = 0`
- **Order:** `ORDER BY total_bytes DESC` (process largest tables first)
- **Inner table enrichment:** `enrichTablesByInnerDependencies()` finds missing `.inner.` and `.inner_id.` tables for materialized views
- **Version-specific fixes:** `fixVariousVersions()` backfills empty CreateTableQuery, converts MV CREATE to ATTACH, restores hidden credentials from metadata files (v23.3+), handles UUID zeros

#### Rust Implementation
- **Fixed column set:** Always queries `database, name, engine, create_table_query, toString(uuid) as uuid, data_paths, total_bytes`
- **Excluded databases:** `system`, `INFORMATION_SCHEMA`, `information_schema` (hardcoded WHERE clause)
- **No is_temporary filter**
- **No ORDER BY total_bytes**
- **No inner table enrichment**
- **No version-specific fixes**

#### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 3.1.1 | **No `is_temporary = 0` filter** | MEDIUM | Temporary tables could be included in backup listings. These are session-scoped and backing them up makes no sense. |
| 3.1.2 | **No MySQL/PostgreSQL/MaterializedPostgreSQL database exclusion** | LOW | Go excludes tables from these engine databases. May include unbacked-up tables in listings. Not critical since backup of these engines would likely fail gracefully. |
| 3.1.3 | **No `ORDER BY total_bytes DESC`** | LOW | Processing largest tables first gives better progress estimation and earlier failure detection. Not functionally incorrect without it. |
| 3.1.4 | **No inner table enrichment for MVs** | MEDIUM | Go's `enrichTablesByInnerDependencies()` finds `.inner.*` and `.inner_id.*` tables for materialized views and adds them to the table list. Rust relies on the general `list_tables()` which should include these (they appear in system.tables), but there is no explicit verification or ordering guarantee. |
| 3.1.5 | **No version-specific DDL fixes** | MEDIUM | Go's `fixVariousVersions()` handles: (a) empty CreateTableQuery by falling back to SHOW CREATE TABLE, (b) MV CREATE->ATTACH conversion, (c) hidden credential restoration from metadata files for v23.3+, (d) UUID zero handling. Rust assumes CreateTableQuery is always populated and correct. |
| 3.1.6 | **No version compatibility detection** | LOW | Go checks which system.tables columns exist before querying. Rust hardcodes columns. Will fail on very old ClickHouse versions where data_paths or total_bytes don't exist. Acceptable since the project targets CH 21.8+. |

### 3.2 system.disks (GetDisks)

#### Go Implementation
- Queries: `path, name, type, free_space, storage_policies`
- **Disk mapping:** `config.DiskMapping` overrides disk paths
- **Enrichment:** Unmapped config entries become virtual disks
- **Version fallback:** For versions < 19.15, falls back to `system.settings`
- **Column availability check:** Queries `system.columns` to check which disk columns exist

#### Rust Implementation
- Queries: `name, path, type, ifNull(remote_path, '') as remote_path`
- No disk mapping
- No enrichment
- No version fallback
- No column availability check

#### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 3.2.1 | **No `disk_mapping` config support** | MEDIUM | Go allows overriding disk paths via config. Useful when ClickHouse and the backup tool see different filesystem mounts. Rust has no equivalent. |
| 3.2.2 | **No `free_space` in disk query** | LOW | Go queries `free_space` from system.disks (used for disk space checks). Rust has a separate `query_disk_free_space()` method that queries `free_space` -- so this is covered, just via a different query. Not a real gap. |
| 3.2.3 | **No `storage_policies` in disk query** | LOW | Go queries storage_policies. Rust does not. Used in Go for policy mapping but not critical for backup functionality. |

### 3.3 system.parts

#### Go Implementation
- `FreezeTableByParts`: `SELECT DISTINCT partition_id FROM system.parts WHERE database=? AND table=? [AND extra_where]`
- `getTableSizeFromParts`: `SELECT sum(bytes_on_disk) FROM system.parts WHERE active AND database=? AND table=?`
- `CalculateMaxFileSize`: `SELECT max(bytes_on_disk * 1.02) FROM system.parts WHERE active`

#### Rust Implementation
- `query_system_parts`: `SELECT name, partition_id, active FROM system.parts WHERE database=? AND table=? AND active = 1`
- `query_distinct_partitions`: `SELECT DISTINCT partition_id FROM system.parts WHERE database=? AND table=? AND active = 1 [AND extra_where] ORDER BY partition_id`

#### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 3.3.1 | **No `CalculateMaxFileSize` equivalent** | LOW | Go uses this to pre-calculate max part size (with 2% buffer) for upload sizing. Rust handles part sizing at the upload layer. Not needed for correctness. |

### 3.4 system.replicas

#### Go Implementation
- `CheckReplicationInProgress`: Queries `log_pointer, log_max_index, absolute_delay, queue_size FROM system.replicas`. Returns false if `log_pointer>2 OR log_max_index>1 OR absolute_delay>0 OR queue_size>0`.
- Checks `CheckReplicasBeforeAttach` config.

#### Rust Implementation
- `check_replica_sync`: Queries `is_readonly, is_session_expired, future_parts, parts_to_check, queue_size, inserts_in_queue, merges_in_queue FROM system.replicas`. Returns true only if all fields are zero.

#### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 3.4.1 | **Different replica sync check columns** | LOW | Go checks `log_pointer`, `log_max_index`, `absolute_delay`, `queue_size`. Rust checks `is_readonly`, `is_session_expired`, `future_parts`, `parts_to_check`, `queue_size`, `inserts_in_queue`, `merges_in_queue`. Rust actually checks MORE fields. Both approaches are valid; Rust's is arguably more thorough. Not a gap -- Rust is stricter. |

### 3.5 system.build_options / version detection

#### Go Implementation
- `GetVersion`: `SELECT value FROM system.build_options WHERE name='VERSION_INTEGER'` -- returns integer version (e.g., 21008000)
- Version cached in `ch.version` field
- Used extensively for conditional behavior throughout the codebase

#### Rust Implementation
- `get_version`: `SELECT version() as version` -- returns string (e.g., "21.8.1.1")
- No version caching
- Minimal version-dependent behavior

#### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 3.5.1 | **No integer version for conditional logic** | LOW | Go uses integer version for many conditional paths (column availability, feature detection). Rust targets CH 21.8+ and avoids most version-dependent code paths. Acceptable given the minimum version requirement. |

---

## 4. DDL Execution

### 4.1 CREATE TABLE

#### Go Implementation
- **ON CLUSTER injection:** Regex-based, handles CREATE TABLE/VIEW/MATERIALIZED VIEW/DICTIONARY
- **UUID handling for Replicated databases:** Checks `database_replicated_allow_explicit_uuid` setting; removes UUID if setting is 0
- **LIVE/WINDOW VIEW analyzer fix:** For v24.3+, disables `allow_experimental_analyzer` setting
- **MATERIALIZED VIEW REFRESH:** Adds `EMPTY` keyword to REFRESH clauses to prevent immediate refresh
- **CREATE -> ATTACH conversion:** For `restore_as_attach` mode
- **Distributed cluster validation:** Checks if original cluster exists in `system.clusters`; substitutes if not

#### Rust Implementation
- **ON CLUSTER injection:** `add_on_cluster_clause()` in `remap.rs` -- string-based, handles CREATE/DROP TABLE/DATABASE/VIEW/MATERIALIZED VIEW/DICTIONARY
- **UUID handling:** `rewrite_create_table_ddl()` removes UUID clause entirely (lets CH assign new)
- **No LIVE/WINDOW VIEW analyzer fix**
- **No REFRESH EMPTY injection**
- **No Distributed cluster validation against system.clusters**

#### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 4.1.1 | **No `database_replicated_allow_explicit_uuid` check** | MEDIUM | Go checks this setting and conditionally removes UUIDs. Rust always removes UUIDs in remap mode but keeps them in non-remap mode. Could cause failures when restoring to a DatabaseReplicated database without remap. |
| 4.1.2 | **No LIVE/WINDOW VIEW experimental_analyzer fix** | LOW | Go adds `SETTINGS allow_experimental_analyzer=0` for these views on v24.3+. Without this, LIVE/WINDOW VIEWs may fail to create on newer CH versions. These are experimental features with limited use. |
| 4.1.3 | **No REFRESH EMPTY for materialized views** | MEDIUM | Go adds `EMPTY` to `REFRESH` clauses to prevent immediate execution during restore. Rust creates refreshable MVs without `EMPTY`, which could trigger refresh queries against incomplete data. However, Rust postpones refreshable MVs to Phase 2b (after data attach), which partially mitigates this. |
| 4.1.4 | **No Distributed cluster existence check** | LOW | Go checks `system.clusters` before using the original cluster name. Rust uses `restore_distributed_cluster` config for explicit override but does not validate the target cluster exists. |

### 4.2 CREATE DATABASE

#### Go Implementation
- `CreateDatabaseWithEngine`: Replaces `{database}` placeholder in engine string for v24.4+
- `CreateDatabaseFromQuery`: Ensures "CREATE DATABASE IF NOT EXISTS" prefix

#### Rust Implementation
- `create_database_with_cluster`: Ensures IF NOT EXISTS, applies ON CLUSTER
- `ensure_if_not_exists_database`: Simple string replacement

#### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 4.2.1 | **No `{database}` placeholder replacement in engine** | LOW | Go handles `{database}` in the engine string for DatabaseReplicated on v24.4+. Rust does not. Could cause issues with newer CH versions where the engine definition uses this placeholder. |

### 4.3 DROP TABLE / DROP DATABASE

Both Go and Rust support Mode A (--rm) with DROP TABLE/DATABASE, ON CLUSTER, retry loops, and system database protection. Rust implementation is equivalent.

No significant gaps.

---

## 5. ZooKeeper Queries

### Go Implementation
- `AttachTable` method extracts ZK path/replica from DDL, applies macros, drops replica from ZK
- Uses regex for ZK parameter extraction
- Applies `ApplyMacros()` which queries `system.macros` for substitution

### Rust Implementation
- `resolve_zk_conflict()` in `schema.rs` does the same flow
- `parse_replicated_params()` extracts ZK path/replica from DDL using string parsing
- `resolve_zk_macros()` substitutes macro placeholders
- `check_zk_replica_exists()` queries `system.zookeeper`
- `drop_replica_from_zkpath()` executes `SYSTEM DROP REPLICA`

### Gaps

No significant gaps. Both implementations follow the same flow.

---

## 6. Macros Resolution

### Go Implementation
- `ApplyMacros()`: Queries `system.macros`, caches result, substitutes `{macro_name}` patterns in strings
- `ApplyMacrosToObjectLabels()`: Extends with backup-specific substitutions: `{backup}`, `{backupName}`, `{backup_name}`, `{BACKUP_NAME}`
- Used in: ZK path resolution, object storage labels, S3 prefix expansion

### Rust Implementation
- `get_macros()`: Queries `system.macros`, returns HashMap (no caching, re-queries each call)
- `resolve_zk_macros()`: Substitutes `{key}` patterns from macro map
- Used in: ZK conflict resolution, watch name templates

### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 6.1 | **No macro caching** | LOW | Go caches macros in `ch.version`-like pattern. Rust re-queries each call. Minor performance impact since macros are queried at most once per command invocation. |
| 6.2 | **No backup-specific macro substitution** | LOW | Go's `ApplyMacrosToObjectLabels()` handles `{backup}`, `{backupName}` etc. Rust does not use these. Only relevant for S3 object labels which Rust does not implement. |

---

## 7. Database Engine Detection

### Go Implementation
- Queries `SHOW CREATE DATABASE` for the engine info
- Also parses engine from metadata files for v23.3+ (credential unmasking)
- Extensive handling of DatabaseReplicated

### Rust Implementation
- `query_database_engine()`: `SELECT engine FROM system.databases WHERE name = '...'`
- Returns empty string if not found
- Used to detect "Replicated" engine for ON CLUSTER skipping

### Gaps

No significant functional gaps. Rust's approach is simpler but sufficient for the needed detection.

---

## 8. Parts Column Consistency Check

### Go Implementation
- `CheckSystemPartsColumnsForTables()`:
  ```sql
  SELECT database, table, column, groupUniqArray(type) AS uniq_types
  FROM system.parts_columns
  WHERE active AND (database='x' AND table='y') [OR ...]
  AND type NOT LIKE 'Enum%(%'
  AND type NOT LIKE 'Tuple(%'
  AND type NOT LIKE 'Nullable(Enum%(%'
  AND type NOT LIKE 'Nullable(Tuple(%'
  AND type NOT LIKE 'Array(Tuple(%'
  AND type NOT LIKE 'Nullable(Array(Tuple(%'
  GROUP BY database, table, column
  HAVING length(uniq_types) > 1
  ```
- Post-query normalization: strips `LowCardinality(...)`, `Nullable(...)` wrappers
- Handles `AggregateFunction`/`SimpleAggregateFunction` version differences
- Handles `Date` parameter variations

### Rust Implementation
- `check_parts_columns()`:
  ```sql
  SELECT database, table, name AS column, groupUniqArray(type) AS uniq_types
  FROM system.parts_columns
  WHERE active AND (database, table) IN (...)
  GROUP BY database, table, column
  HAVING length(uniq_types) > 1
  ```
- Post-query filtering in Rust code: filters types containing "Enum", "Tuple", "Nullable", "Array(Tuple"

### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 8.1 | **Type exclusion in SQL vs post-filter** | LOW | Go excludes benign types in the SQL WHERE clause (more efficient). Rust fetches all inconsistencies and filters in application code. Both produce the same result. Minor efficiency difference. |
| 8.2 | **No LowCardinality/Nullable stripping** | LOW | Go strips `LowCardinality(X)` -> `X` and `Nullable(X)` -> `X` wrappers before comparison. Rust's post-filter on keywords achieves similar effect but is less precise. Could report false-positive inconsistencies where the only difference is a LowCardinality wrapper. |
| 8.3 | **No AggregateFunction version normalization** | LOW | Go normalizes AggregateFunction version differences. Rust does not. Could report false-positive inconsistencies for tables with AggregateFunction columns across different part versions. |

---

## 9. Query Settings and Timeouts

### Go Implementation
- Connection-level settings:
  - `connect_timeout`: from config (default 30m, parsed as duration)
  - `receive_timeout`: same
  - `send_timeout`: same
  - `http_send_timeout`: 300 seconds (fixed)
  - `http_receive_timeout`: 300 seconds (fixed)
- No per-query `max_execution_time`
- `ConnMaxLifetime`: 0 (never expire -- important for RBAC operations)

### Rust Implementation
- No connection-level timeout settings
- No per-query timeout settings
- HTTP client defaults from `clickhouse-rs`/hyper

### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 9.1 | **No timeout configuration** | HIGH | This is the same as gap 1.1 but worth emphasizing: without timeouts, long-running operations (SYNC REPLICA on a table with millions of parts, DDL on a cluster with a slow node) can hang the process indefinitely. The Go default of 30 minutes is generous but prevents infinite hangs. |

---

## 10. Error Handling

### Go Implementation
- Error codes checked explicitly:
  - 60 (UNKNOWN_TABLE): Freeze, Drop
  - 81 (UNKNOWN_DATABASE): Freeze
  - 218 (CANNOT_FREEZE_PARTITION): Freeze
- ATTACH PART errors: Not specifically documented but handled
- Wrapped errors with context

### Rust Implementation
- Error codes checked via string matching:
  - "UNKNOWN_TABLE" / "Code: 60": Freeze
  - "UNKNOWN_DATABASE" / "Code: 81": Freeze
  - "Code: 218": Freeze-by-part
  - Codes 232/233 (overlapping/already exists): ATTACH PART
- Uses `anyhow::Result` with `.context()` chains

### Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 10.1 | **String-based error code matching** | LOW | Go uses typed error code checks from the driver. Rust matches on string representation. Works but is fragile -- error message format changes could break detection. Consider using the `clickhouse-rs` error type's code field if available. |

---

## 11. Features Present in Go but Missing in Rust

### 11.1 XML Configuration Parsing

Go has `ParseXML()` and `GetPreprocessedXMLSettings()` for reading ClickHouse's preprocessed XML config files with retry logic. Used for:
- Determining `access_control_path` for RBAC
- Extracting configuration that may differ from runtime settings
- Reading hidden credentials from metadata files

Rust does not parse ClickHouse XML configs. This limits the ability to detect certain ClickHouse configurations.

**Severity:** LOW -- Most needed settings are queryable via system tables.

### 11.2 Embedded Backup Mode (UseEmbeddedBackupRestore)

Go supports `UseEmbeddedBackupRestore` config which uses ClickHouse's built-in BACKUP/RESTORE commands (v22.8+).

Rust does not implement this mode.

**Severity:** LOW -- This is an alternative backup strategy, not a required feature. The manual FREEZE-based approach is more portable and the primary method.

### 11.3 Access Management Path Detection

Go's `GetAccessManagementPath()` uses multiple fallback strategies:
1. Query `system.user_directories`
2. Parse preprocessed config.xml for `access_control_path`
3. Scan disks for `access/` subdirectory
4. Default to `/var/lib/clickhouse/access`

Rust's RBAC backup/restore uses config-derived paths.

**Severity:** LOW -- The default path works in most deployments. Config-driven approach is simpler and sufficient.

### 11.4 Version-Specific DDL Fixes

Go's `fixVariousVersions()` handles:
- Empty `CreateTableQuery` (backfill via SHOW CREATE TABLE)
- MaterializedView CREATE -> ATTACH conversion
- Hidden credential restoration from metadata files (v23.3+)
- UUID zero handling (v20.6.3.28)

Rust has none of these fixes.

**Severity:** MEDIUM -- The hidden credential restoration is the most impactful. In ClickHouse v23.3+, `system.tables.create_table_query` masks credentials (passwords in engine parameters). The Go code reads the original DDL from metadata files on disk. Without this, restored tables may have masked credentials, breaking connections to external systems.

### 11.5 Inner Table Dependencies for Materialized Views

Go's `enrichTablesByInnerDependencies()` explicitly finds and adds `.inner.` and `.inner_id.` tables that back materialized views.

Rust relies on `list_tables()` returning these from `system.tables` naturally.

**Severity:** LOW -- Inner tables should appear in system.tables. The explicit enrichment in Go is a safety measure for edge cases.

### 11.6 CalculateMaxFileSize

Go pre-calculates maximum part size for upload buffer sizing with a 2% safety margin.

Rust handles part sizing at the upload layer.

**Severity:** LOW -- Not needed for correctness. May affect memory efficiency in edge cases.

---

## 12. Summary of Gaps by Priority

### HIGH Priority (potential hangs or data issues)

| # | Gap | Recommendation |
|---|-----|----------------|
| 1.1/9.1 | No connection/query timeout | Add `connect_timeout`, `receive_timeout`, `send_timeout` settings to the clickhouse-rs client. The clickhouse-rs crate supports setting query-level settings via `.with_option()`. At minimum, set a 30-minute default timeout. |

### MEDIUM Priority (functional gaps in specific scenarios)

| # | Gap | Recommendation |
|---|-----|----------------|
| 1.2 | No connection retry | Add retry loop with configurable delay in `ChClient::new()` or a separate `connect_with_retry()` method. Important for container orchestration where CH may start after the backup sidecar. |
| 1.4 | TLS client certs not supported | Investigate whether `clickhouse-rs` supports custom TLS config or consider switching to `reqwest` with custom client. Alternatively, document the limitation. |
| 1.5 | skip_verify not functional | Same as 1.4 -- investigate TLS config options in the HTTP client. |
| 2.1 | No `tuple()` for unpartitioned tables | Add special case: when partition_id is "all", use `FREEZE PARTITION tuple()` syntax. |
| 3.1.1 | No is_temporary filter | Add `AND is_temporary = 0` to system.tables queries. Simple fix. |
| 3.1.4 | No inner table enrichment | Verify that inner tables are included via normal list_tables() query. If not, add explicit enrichment. |
| 3.1.5 | No version-specific DDL fixes | Implement credential unmasking for v23.3+ (most impactful). The SHOW CREATE TABLE fallback is also valuable for robustness. |
| 3.2.1 | No disk_mapping config | Add `disk_mapping: HashMap<String, String>` to ClickHouseConfig and apply in `get_disks()`. Important for deployments with different mount points. |
| 4.1.1 | No UUID removal for DatabaseReplicated | Check `database_replicated_allow_explicit_uuid` setting before creating tables in Replicated databases. |
| 4.1.3 | No REFRESH EMPTY for MVs | Add `EMPTY` keyword to REFRESH clauses during restore. Partially mitigated by Phase 2b postponement. |
| 11.4 | No credential unmasking in DDL | For CH v23.3+, read original DDL from metadata files on disk to preserve credentials. This is important for tables with engine parameters containing passwords (e.g., MySQL, PostgreSQL, S3 engines). |

### LOW Priority (minor differences, acceptable for now)

| # | Gap |
|---|-----|
| 1.3 | No server-side log_queries setting |
| 1.6 | Default port mismatch (documentation) |
| 3.1.2 | No MySQL/PostgreSQL database exclusion |
| 3.1.3 | No ORDER BY total_bytes DESC |
| 3.1.6 | No version compatibility detection |
| 3.2.3 | No storage_policies in disk query |
| 3.3.1 | No CalculateMaxFileSize |
| 3.5.1 | No integer version for conditional logic |
| 4.1.2 | No LIVE/WINDOW VIEW analyzer fix |
| 4.1.4 | No Distributed cluster existence check |
| 4.2.1 | No {database} placeholder in engine |
| 6.1 | No macro caching |
| 6.2 | No backup-specific macro substitution |
| 8.1 | Type exclusion efficiency |
| 8.2 | No LowCardinality/Nullable stripping |
| 8.3 | No AggregateFunction normalization |
| 10.1 | String-based error code matching |
| 11.1 | No XML config parsing |
| 11.2 | No embedded backup mode |
| 11.3 | No access management path detection |
| 11.5 | No explicit inner table enrichment |
| 11.6 | No CalculateMaxFileSize |

---

## 13. Protocol Difference: Native vs HTTP

The most fundamental architectural difference is that Go uses ClickHouse's **native TCP protocol** (port 9000) while Rust uses the **HTTP protocol** (port 8123) via the `clickhouse-rs` crate.

Implications:
- **Performance:** Native protocol is generally faster for large result sets (binary format vs JSON/TSV over HTTP). Not significant for backup operations which are I/O-bound.
- **Connection pooling:** Native protocol requires explicit pool management. HTTP is stateless, simpler.
- **Timeout behavior:** Native protocol timeouts are connection-level. HTTP timeouts can be set per-request.
- **Feature parity:** Both protocols support all the SQL operations used by backup tools.
- **TLS:** Native protocol has its own TLS stack. HTTP uses standard HTTPS. Both are well-supported.

This is a deliberate design choice, not a gap. The HTTP protocol is simpler and the `clickhouse-rs` crate is well-maintained. The key gap is ensuring HTTP-level timeouts are properly configured (see gap 1.1/9.1).
