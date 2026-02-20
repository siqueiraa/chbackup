# Restore Flow: Go vs Rust Parity Gap Analysis

Comparison of `Altinity/clickhouse-backup` (Go) restore flow against `chbackup` (Rust)
restore flow. Go source: `pkg/backup/restore.go`. Rust source: `src/restore/`.

## Summary

The Rust implementation covers the majority of the Go restore features. The gaps
identified are primarily around features that were deliberately out of scope (embedded
backups, GCS/Azure storage, encrypted named collections) or represent minor behavioral
differences. Several Go features are fully implemented in Rust with equivalent or
superior architecture.

---

## Feature Comparison Matrix

| Feature | Go | Rust | Status |
|---|---|---|---|
| Mode A (--rm DROP) | Yes | Yes | PARITY |
| Mode B (non-destructive) | Yes | Yes | PARITY |
| CREATE DATABASE DDL | Yes | Yes | PARITY |
| CREATE TABLE DDL | Yes | Yes | PARITY |
| ON CLUSTER support | Yes | Yes | PARITY |
| ZK conflict resolution | Yes | Yes | PARITY |
| ATTACH TABLE mode (restoreAsAttach) | Yes | Yes | PARITY |
| schemaAsAttach (ATTACH instead of CREATE for views) | Yes | No | GAP |
| replicatedCopyToDetached | Yes | No | GAP |
| Mutation re-apply | Yes | Yes | PARITY |
| Partition filtering (--partitions) | Yes | Yes | PARITY |
| Skip empty tables | Yes | Yes | PARITY |
| Check replicas before attach | Yes | Yes | PARITY |
| Distributed cluster rewrite | Yes | Yes | PARITY |
| RBAC restore | Yes | Yes | PARITY |
| Config file restore | Yes | Yes | PARITY |
| Named collections restore | Yes | Yes | MINOR GAP |
| Restart command execution | Yes | Yes | PARITY |
| Database mapping (-m) | Yes | Yes | PARITY |
| Table mapping | Yes | No | GAP |
| Table rename (--as) | Yes | Yes | PARITY |
| Resume state | Yes | Yes | PARITY |
| Parallel restore | Yes | Yes | PARITY |
| Topological sort / dependency ordering | Yes | Yes | PARITY |
| Streaming engine postponement | Yes | Yes | PARITY |
| Refreshable MV postponement | Yes | Yes | PARITY |
| Object disk (S3) part restore | Yes | Yes | PARITY |
| UUID-isolated S3 restore | Yes | Yes | PARITY |
| Same-name optimization for S3 | Yes | Yes | PARITY |
| Object disk key rewriting (for remap) | Yes | Yes | PARITY |
| Embedded backup restore | Yes | No | OUT OF SCOPE |
| Ignore dependencies flag | Yes | No | GAP |
| rbacOnly / configsOnly flags | Yes | No | MINOR GAP |
| AllowEmptyBackups config | Yes | No | GAP |
| force_drop_table flag file | Yes | No | MINOR GAP |
| dropExistPartitions (data-only + partitions) | Yes | No | GAP |
| UDF restore (user-defined functions) | Yes | Yes | PARITY |
| Named collections decryption (AES-CTR) | Yes | No | OUT OF SCOPE |
| Replicated RBAC via ZooKeeper | Yes | No | GAP |
| Skip projections on restore | Via backup | Via backup | PARITY |
| ClickHouse version-gated behavior | Yes | Partial | MINOR GAP |

---

## Detailed Gap Analysis

### 1. schemaAsAttach Flag

**Go behavior**: When `schemaAsAttach=true`, transforms `CREATE TABLE` to `ATTACH TABLE`
for MaterializedView, WindowView, and LiveView specifically during schema restore. This
preserves the existing table metadata on disk without recreating it.

**Rust status**: Not implemented. The Rust `restoreAsAttach` config applies only to
Replicated*MergeTree tables during the data restore phase (DETACH/ATTACH TABLE flow).
There is no mechanism to convert CREATE to ATTACH for views during schema restore.

**Impact**: Low. This is a niche feature for scenarios where view metadata files already
exist on disk. Most users use `--rm` or non-destructive mode.

**Effort**: Low. Would require adding a DDL string replacement in `create_tables()` or
`create_ddl_objects()` for view-type engines when the flag is set.

---

### 2. replicatedCopyToDetached Flag

**Go behavior**: When `replicatedCopyToDetached=true`, data is copied/hardlinked to the
`detached/` directory for Replicated*MergeTree tables, but `ATTACH TABLE` / `ATTACH PART`
is deliberately **skipped**. The user must manually verify and attach parts later.

**Rust status**: Not implemented as a CLI flag or config option.

**Impact**: Low-medium. This is a safety net for operators who want to manually inspect
data before attaching in replicated clusters. Uncommon in production automation.

**Effort**: Low. Would require adding a `--replicated-copy-to-detached` CLI flag and
skipping the ATTACH step in `attach_parts_inner()` when the flag is set and engine is
Replicated.

---

### 3. Table Mapping (RestoreTableMapping)

**Go behavior**: Supports `RestoreTableMapping` config (format: `sourceTable:destTable` or
`sourceDb.sourceTable:destDb.destTable`) allowing bulk table renaming beyond single-table
`--as`. Builds reverse mapping for backup file lookups.

**Rust status**: Only single-table rename via `--as` flag and database-level mapping via
`-m` are implemented. No bulk table-level mapping.

**Impact**: Low. The `--as` flag covers the single-table case, and `-m` covers bulk
database renames. Bulk table-level renaming is very rarely used.

**Effort**: Medium. Would require extending `RemapConfig` with a `table_mapping: HashMap`
and updating `remap_table_key()` priority chain.

---

### 4. Ignore Dependencies Flag (--ignore-dependencies)

**Go behavior**: When `ignoreDependencies=true` and ClickHouse >= 21.1, adds
`SETTINGS check_table_dependencies=0` to DROP statements, allowing tables to be dropped
even if other tables depend on them.

**Rust status**: Not implemented. The retry loop in `drop_tables()` handles dependency
failures via retries, but there is no way to bypass the check.

**Impact**: Low-medium. The retry-based approach works for most cases (tables that depend
on the one being dropped get dropped first in subsequent rounds). The
`check_table_dependencies=0` setting is a stronger override.

**Effort**: Low. Add `--ignore-dependencies` flag, pass through to `drop_tables()`, append
`SETTINGS check_table_dependencies=0` to DROP DDL.

---

### 5. AllowEmptyBackups Config

**Go behavior**: When `AllowEmptyBackups=true`, the restore proceeds without error even if
the backup contains no tables or data. When false, returns error "doesn't contain tables
for restore".

**Rust status**: Not implemented. The Rust code returns early with a warning log when
`table_keys.is_empty()` but does not error out -- so it is partially more permissive by
default. However, there is no explicit config toggle.

**Impact**: Low. Rust behavior is already more lenient (warns but succeeds). Adding the
config option would be for strict parity only.

**Effort**: Trivial. Add `allow_empty_backups` to config, change the early return to an
error when false and table_keys is empty.

---

### 6. dropExistPartitions (data-only + partitions)

**Go behavior**: When restoring with `--data-only` AND `--partitions`, before attaching
parts, the Go code drops existing partitions that overlap with the restore set via
`ALTER TABLE DROP PARTITION`. This prevents duplicate/conflicting data.

**Rust status**: Not implemented. The Rust `--partitions` on restore only filters which
parts to ATTACH, but does not pre-drop existing partitions.

**Impact**: Medium. Without dropping existing partitions first, a data-only restore with
`--partitions` could result in duplicate data if the partition already has data. Users
working with `--data-only --partitions` would need to manually drop partitions.

**Effort**: Low-medium. Add a pre-flight step in the restore flow that, when `data_only &&
!partition_filter.is_empty()`, iterates data tables and executes
`ALTER TABLE DROP PARTITION` for each partition in the filter.

---

### 7. rbacOnly / configsOnly Flags

**Go behavior**: Has `rbacOnly` and `configsOnly` flags that restore ONLY RBAC or configs,
skipping all table schema and data. This is useful for operators who want to restore access
controls independently.

**Rust status**: Not implemented as distinct flags. RBAC and config restore are always
combined with table restore via `--rbac` and `--configs` flags.

**Impact**: Low. The existing `--rbac --schema` combination achieves a similar result
(restores RBAC with schema but no data). Pure RBAC-only restore is a convenience.

**Effort**: Low. Add `--rbac-only` and `--configs-only` flags that short-circuit the
restore flow to only run RBAC/config phases.

---

### 8. force_drop_table Flag File

**Go behavior**: When `schemaOnly && dropExists`, creates
`{data_path}/flags/force_drop_table` file before executing `DROP DATABASE IF EXISTS`. This
is a ClickHouse internal mechanism that signals the server to force-drop tables even if
they have dependencies.

**Rust status**: Not implemented. The Rust code relies on the retry loop for dependency
resolution during drops.

**Impact**: Low. The retry loop handles dependencies in practice. The flag file is a
belt-and-suspenders approach for edge cases.

**Effort**: Trivial. Create the flag file before drop operations, remove it after.

---

### 9. Named Collections Decryption

**Go behavior**: Implements AES-CTR decryption with SipHash128/SipHash64 key fingerprint
verification for encrypted named collections stored in Keeper (ZooKeeper). Supports
AES-128/192/256 key lengths and both v1/v2 encryption formats.

**Rust status**: Not implemented. The Rust code restores named collections from plain DDL
in the manifest. Encrypted named collections from Keeper nodes are not supported.

**Impact**: Low. Encrypted named collections require a specific ZooKeeper/Keeper setup
with encryption keys. Most deployments use SQL-based named collections that don't need
decryption.

**Effort**: High. Would require implementing SipHash128, AES-CTR cipher, and the
ClickHouse-specific encryption format parser.

---

### 10. Replicated RBAC via ZooKeeper

**Go behavior**: Queries `system.user_directories` for replicated user directories,
connects to Keeper (ZooKeeper proxy), and restores RBAC objects directly into the Keeper
nodes. Also handles `.jsonl` keeper dump files.

**Rust status**: Not implemented. The Rust RBAC restore only handles SQL-based .jsonl
files and executes DDL statements directly.

**Impact**: Low. Replicated RBAC via ZooKeeper is a newer ClickHouse feature not widely
adopted. SQL-based RBAC restore covers the vast majority of deployments.

**Effort**: High. Would require implementing a ZooKeeper/Keeper client and the node
manipulation logic.

---

### 11. ClickHouse Version-Gated Behavior

**Go behavior**: Checks ClickHouse version for feature gating:
- `>= 21.1`: `check_table_dependencies` setting support
- `>= 19.17`: `mutations_sync` setting support
- `>= 20.8`: database engine support in CREATE DATABASE
- `>= 23.9`: `{uuid}` macro handling change

**Rust status**: Partial. The Rust code does not explicitly check ClickHouse version for
feature gating. It assumes modern ClickHouse (21.8+) per the design doc. However,
`mutations_sync=2` is always used (correct for 21.8+), and database engines are always
created with their engine clause.

**Impact**: Low for the 21.8+ requirement. Only affects users running unsupported older
versions.

**Effort**: Low. Would require querying ClickHouse version at restore start and
conditionally adjusting DDL/settings.

---

### 12. Embedded Backup Restore

**Go behavior**: Supports restoring from backups created with ClickHouse's native `BACKUP`
command. Uses `RESTORE FROM` SQL, handles metadata fixup for `{uuid}` macros, empty
ReplicatedMergeTree parameters, and MATERIALIZED VIEW EMPTY keyword injection.

**Rust status**: Not implemented. By design, chbackup creates its own backup format (FREEZE
+ shadow walk) and does not support ClickHouse native BACKUP/RESTORE SQL.

**Impact**: N/A. This is an architectural decision. Embedded backups are a different
paradigm from chbackup's approach.

**Effort**: Very high. Would require a fundamentally different restore path.

---

## Features Where Rust Implementation is Superior

### Topological Sort with Dependency Graph
Go uses a simple retry loop for dependency ordering. Rust uses Kahn's algorithm with
engine-priority tie-breaking and cycle detection, which is more deterministic and produces
better ordering.

### Streaming Engine Postponement
Rust explicitly classifies Kafka, NATS, RabbitMQ, S3Queue, and refreshable MVs into a
postponed phase (Phase 2b) that runs after data attachment. Go's ordering is less explicit
about preventing premature data consumption.

### Resume State Architecture
Rust uses a shared `Arc<Mutex<(RestoreState, PathBuf)>>` with per-part atomic saves,
graceful degradation on write failure, and authoritative system.parts merging. The
architecture is well-suited for parallel restore tasks.

### Phase Classification
Rust explicitly classifies tables into data_tables, postponed_tables, and ddl_only_tables
with a documented priority decision tree. Go's classification is more implicit.

---

## Recommended Priority for Closing Gaps

### High Priority (user-facing behavior gaps)
1. **dropExistPartitions** - Affects correctness of `--data-only --partitions` restore.
   Users could get duplicate data without this.

### Medium Priority (feature completeness)
2. **AllowEmptyBackups** - Simple config flag for edge case handling
3. **Ignore dependencies flag** - Useful for complex DROP scenarios

### Low Priority (niche features)
4. **schemaAsAttach** - Niche view metadata preservation
5. **replicatedCopyToDetached** - Manual verification flow
6. **Table mapping** - Bulk table rename (rare)
7. **rbacOnly/configsOnly** - Convenience flags
8. **force_drop_table flag file** - Belt-and-suspenders for drops

### Out of Scope
9. **Embedded backup restore** - Different paradigm
10. **Named collections decryption** - Requires crypto implementation
11. **Replicated RBAC via ZooKeeper** - Requires Keeper client

---

## Appendix: Restore Phase Comparison

### Go Restore Flow
```
1. PID lock
2. Connect to ClickHouse, get version
3. List local backups (if no name given)
4. Load metadata
5. Restore empty databases
6. Restore RBAC (if flag)
7. Restart ClickHouse (if RBAC/configs changed)
8. Restore configs (if flag)
9. Restore named collections (if flag)
10. Filter tables by pattern + partitions
11. Capture existingTablesSnapshot
12. Restore schema (if not data-only):
    - Drop existing tables (if --rm)
    - Create databases + tables (with retry for deps)
13. Drop existing partitions (if data-only + partitions)
14. Restore data (if not schema-only):
    - Parallel by table, per-table sequential
    - Attach or parts-based method
    - Apply mutations
15. Restore functions
16. Clean up required backups
17. Log completion
```

### Rust Restore Flow
```
1. Load manifest
2. Filter tables by pattern
3. Parse partition filter
4. Build remap config
5. Classify tables into restore phases
6. Detect ON CLUSTER and DatabaseReplicated
7. Get macros for ZK resolution
Phase 0: DROP tables/databases (Mode A only)
Phase 1: CREATE databases
Phase 2: CREATE + ATTACH data tables
  - ZK conflict resolution
  - Parallel per-table ATTACH
  - ATTACH TABLE mode for Replicated (if configured)
Phase 2.5: Mutation re-apply
Phase 2b: CREATE postponed tables (streaming engines, refreshable MVs)
Phase 3: CREATE DDL-only objects (topologically sorted)
Phase 4: CREATE functions
Phase 4b: Restore named collections
Phase 4c: Restore RBAC
Phase 4d: Restore config files
Phase 4e: Execute restart commands
8. Delete resume state
9. Log summary
```

### Key Ordering Differences

| Aspect | Go | Rust |
|---|---|---|
| RBAC restore timing | Before table schema | After all table schema |
| Config restore timing | Before table schema | After all table schema |
| Named collections | Before table schema | After functions |
| ClickHouse restart | After RBAC/configs, before tables | After RBAC/configs (end) |
| Functions | After data restore | Phase 4 (after DDL-only) |
| Empty database creation | Dedicated step before schema | Merged into Phase 1 |
| Partition drop | Before data restore (data-only) | Not implemented |

The Go approach of restoring RBAC/configs/named-collections **before** table schema means
that if a table DDL references a named collection, it will already exist. The Rust
approach restores them after, which could cause CREATE TABLE failures if a table references
a named collection that hasn't been restored yet. This is a **subtle ordering bug** in the
Rust implementation.

### Action Item: Named Collections Ordering

Named collections should be restored **before** Phase 2 (CREATE tables) when tables may
reference them in their DDL (e.g., `S3(named_collection='...')`). The current Rust
ordering places named collection restore at Phase 4b (after data attachment), which is too
late.

Similarly, RBAC objects (users/roles) might be referenced in DEFINER clauses of
materialized views. However, this is much less common and ClickHouse typically allows
creation with a missing DEFINER.
