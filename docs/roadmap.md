# chbackup — Implementation Roadmap

Maps the design document (1800 lines, 17 sections) into shippable milestones. Each phase produces a working binary that does something useful. No phase depends on a future phase being complete.

**Estimated timeline**: 6-10 weeks for Phases 0-3 (usable in production). Phase 4 is polish.

---

## Phase 0 — Skeleton

**Goal**: Project compiles, CLI parses, connects to ClickHouse and S3. Does nothing useful yet, but every subsequent phase builds on this foundation.

**Duration**: 3-5 days

### Deliverables

```
chbackup/
├── Cargo.toml              # workspace with subcrates
├── src/
│   ├── main.rs             # CLI entry point (clap)
│   ├── cli.rs              # Command definitions, flag parsing
│   ├── config.rs           # YAML config + env var overlay
│   ├── error.rs            # Error types (thiserror)
│   ├── clickhouse/
│   │   ├── mod.rs
│   │   └── client.rs       # Connection pool (clickhouse, HTTP)
│   ├── storage/
│   │   ├── mod.rs
│   │   └── s3.rs           # S3 client wrapper (aws-sdk-s3)
│   ├── lock.rs             # PID lock (backup-scoped + global)
│   └── logging.rs          # tracing + structured JSON logs
├── config.example.yml
└── tests/
    └── config_test.rs
```

### What to build

| Component | Design section | Details |
|-----------|---------------|---------|
| CLI skeleton | §2 Commands | `clap` derive API. All commands defined as empty subcommands. Parse all flags from the flag reference table. |
| Config loader | §12 Configuration | `serde_yaml` deserialize. All ~40 params. Env var overlay: `S3_BUCKET` → `s3.bucket`. Validate: `full_interval > watch_interval`, concurrency > 0, etc. |
| `default-config` | §2 | Print default config YAML to stdout. First working command. |
| `print-config` | §2 | Print resolved config after env var overlay. Essential for debugging. |
| ClickHouse client | §11.3 | `clickhouse` (HTTP) async connection. Test: `SELECT 1`. Connection pool config from `max_connections`. Default port 8123. |
| S3 client | §11.3 | `aws-sdk-s3` with tokio. Test: `ListObjectsV2` on configured bucket/prefix. Handle `force_path_style`, `endpoint`, `assume_role_arn`, `disable_ssl`. |
| PID lock | §2 (lock paragraph) | Three tiers: backup-scoped (`/tmp/chbackup.{name}.pid`), global (`/tmp/chbackup.global.pid`), none. Check if PID alive on acquire. |
| Logging | — | `tracing` with `tracing-subscriber`. JSON format for server mode, human format for CLI. Log level from config or `RUST_LOG`. |
| Error types | — | `thiserror` enum: `ClickHouseError`, `S3Error`, `ConfigError`, `LockError`, `IoError`. All propagate with context via `anyhow` at the binary boundary. |

### Definition of done

```bash
cargo build --release                    # compiles, static musl binary
chbackup default-config                  # prints valid YAML
chbackup create --help                   # shows all flags
chbackup list                            # connects to CH, connects to S3, returns empty list
```

### Design sections consumed: §2 (partial), §11.3 (crate choices), §12 (config)

---

## Phase 1 — MVP: Single-Table Backup & Restore

**Goal**: End-to-end backup and restore of a single table on local disk. No parallelism, no S3 disk, no incrementals. You can `create → upload → download → restore` and get your data back.

**Duration**: 7-10 days

### Deliverables

| Command | What it does | Design section |
|---------|-------------|---------------|
| `create` | FREEZE one table → walk shadow → hardlink to staging → CRC64 → UNFREEZE → write local manifest | §3.1 (mutations, single query), §3.2 (SYNC REPLICA), §3.4 (FREEZE, sequential), §3.5 (no diff yet) |
| `upload` | Read local manifest → streaming compress → S3 PutObject → upload manifest last | §3.6 (single-threaded, sequential) |
| `download` | Download manifest → streaming decompress → write to local backup dir | §4 (single-threaded) |
| `restore` | Read local manifest → hardlink parts to detached/ → ATTACH PART → chown | §5.1 (Phase 2 only), §5.2 (Mode B only), §5.3 (sequential) |
| `list` | Local: scan backup dirs. Remote: list S3 prefixes, download manifests | — |
| `delete` | Local: rm -rf. Remote: delete all S3 keys under prefix | — |

### What to build

```
src/
├── backup/
│   ├── mod.rs
│   ├── freeze.rs           # FREEZE + shadow walk + UNFREEZE
│   ├── mutations.rs        # Batch mutation check
│   ├── sync_replica.rs     # SYSTEM SYNC REPLICA
│   ├── checksum.rs         # CRC64 of checksums.txt
│   └── collect.rs          # Walk shadow, build part list
├── upload/
│   ├── mod.rs
│   └── stream.rs           # file → lz4 compress → S3 PUT (streaming pipeline)
├── download/
│   ├── mod.rs
│   └── stream.rs           # S3 GET → lz4 decompress → file (streaming)
├── restore/
│   ├── mod.rs
│   ├── schema.rs           # CREATE TABLE from DDL
│   ├── attach.rs           # Hardlink + ATTACH PART + chown
│   └── sort.rs             # SortPartsByMinBlock
├── manifest.rs             # Serialize/deserialize manifest JSON (§7)
├── list.rs                 # Local + remote listing
└── table_filter.rs         # Glob pattern matching for -t flag
```

### Key implementation details

**Manifest format** (§7): Define Rust structs with `serde`. Include `manifest_version: 1` from day one. Every field from the JSON example in §7. Use `#[serde(skip_serializing_if = "Option::is_none")]` for optional fields.

**Buffered upload pipeline** (§3.6): Phase 1 uses in-memory buffering for simplicity. Each data part is archived (sync `tar`) and compressed (sync `lz4_flex`) into a `Vec<u8>` via `spawn_blocking`, then uploaded via single `PutObject` with known `Content-Length`. Phase 2 introduces true streaming multipart upload for large parts (>32MB uncompressed per §3.6).
```
Phase 1: files → tar::Builder → lz4_flex::FrameEncoder → Vec<u8> → S3 PutObject
Phase 2: files → tar stream → lz4 stream → S3 MultipartUpload (streaming)
```

**Streaming download** (§4): Reverse direction.
```
S3 GetObject body → lz4_decoder → tar_extract → files on disk
```

**FREEZE naming** (§3.4): `chbackup_{backup_name}_{db}_{table}`. Sanitize special chars to underscores. Implement the scopeguard pattern for UNFREEZE — even in Phase 1 with no parallelism, because it establishes the pattern.

**Restore hardlink** (§5.3): `std::os::unix::fs::hard_link()`. Catch `EXDEV` (cross-device) → fall back to copy. Chown via `nix::unistd::chown()`. Detect target uid/gid by `stat()`-ing the CH data path.

**SortPartsByMinBlock** (§5.3): Parse part name `{partition}_{min}_{max}_{level}`, sort by (partition, min). Sequential ATTACH only in Phase 1.

**ignore_not_exists_error_during_freeze** (§3.4): Catch ClickHouse error codes 60 (UNKNOWN_TABLE) and 81 (DATABASE_NOT_FOUND) during FREEZE. Log warning, skip table, continue. Tables can be DROPped between gathering the list and executing FREEZE — common in production.

**allow_empty_backups** (§3.4): When table filter matches zero tables: if `false` (default) → error. If `true` → create backup with metadata only and zero tables. Implement from day one — affects create flow control.

**log_sql_queries** (§12): When true, log all SQL sent to ClickHouse at info level. When false, log at debug. Essential for debugging — implement in the CH client wrapper in Phase 1.

**Manifest upload last** (§3.6): In Phase 1, simple PutObject (no temp+copy atomicity yet). Broken detection relies on manifest presence.

### Tests

Integration test using the all-in-one Docker container from §1.4.
Tests marked **(impl)** are in `test/run_tests.sh`; unmarked are aspirational future work.

- Round-trip: create → upload → delete local → download → restore **(impl: `test_round_trip`)**
- Restore Mode B (safe ATTACH, table doesn't exist) — covered by `test_round_trip`
- Empty backup handling (allow_empty_backups true/false) — aspirational
- Table dropped during FREEZE (error code 60/81 — ignore_not_exists) — aspirational
- Basic CRC64 checksum computation — covered by `test_round_trip`

### Definition of done

```bash
# Create backup of one table
chbackup create -t default.trades daily_test

# Upload to S3
chbackup upload daily_test

# List shows it
chbackup list remote
# daily_test  2025-02-15  134.2 MB  1 tables

# Download to different local dir
chbackup download daily_test

# Restore (drop original first for testing)
clickhouse-client -q "DROP TABLE default.trades"
chbackup restore daily_test

# Verify data matches
clickhouse-client -q "SELECT count() FROM default.trades"
# Same count as before backup
```

### Design sections consumed: §3.1-3.4 (sequential), §3.6 (sequential), §4 (sequential), §5.1-5.3 (Mode B, local disk), §7

---

## Phase 2 — Production: Parallelism, Incrementals, S3 Disk, Resume

**Goal**: Everything needed for a real production backup. Multi-table parallel operations, incremental backups with CRC64, S3 object disk support, multipart upload, resumable operations. After Phase 2, the tool is feature-competitive with the Go tool for the common case.

**Duration**: 10-14 days

### 2a — Parallelism

| Component | Design section | Details |
|-----------|---------------|---------|
| Flat semaphore model | §11.1 | `Arc<Semaphore>` for upload, download, FREEZE, restore. All parts across all tables share one semaphore per operation type. |
| Parallel FREEZE | §3.4 | Pre-flight sync phase (bounded by `max_connections`), then FREEZE phase (same semaphore). Scopeguard UNFREEZE on every task. |
| Parallel upload | §3.6 | Collect all work items, `tokio::spawn` each through upload semaphore. `try_join_all` for fail-fast. |
| Parallel download | §4 | Same pattern as upload. Metadata phase unbounded (tiny JSON), data phase through download semaphore. |
| Parallel restore | §5.3 | Tables in parallel (max_connections). ATTACH within table: parallel for plain MergeTree, sequential sorted for Replacing/Collapsing. |
| Multipart upload | §3.6 | Parts with `uncompressed_size > 32MB` use multipart. `AbortMultipartUpload` in scopeguard on failure. |
| Rate limiting | §3.6 | Token bucket wrapping the byte stream. Global across all concurrent uploads/downloads. |

### 2b — Incremental Backups

| Component | Design section | Details |
|-----------|---------------|---------|
| `--diff-from` | §3.5 | Load base manifest, name+CRC64 comparison, mark parts as `uploaded` or `carried:base_name`. |
| Self-contained manifest | §3.5 | Manifest lists ALL parts with S3 keys. Carried parts point to original backup's key. No `RequiredBackup` field. |
| `--diff-from-remote` | §2 | Same as diff-from but loads base manifest from S3 instead of local. |
| `create_remote` | §2 | `create` + `upload` in one step. Primary command for watch mode. |

### 2c — S3 Object Disk

| Component | Design section | Details |
|-----------|---------------|---------|
| Metadata parsing | §3.7 | All 5 format versions. Parse object paths, handle InlineData (v4), FullObjectKey (v5). |
| Backup: CopyObject | §3.4 | Server-side copy from data bucket to backup bucket. Separate `object_disk_copy_concurrency` semaphore. |
| Restore: UUID isolation | §5.4 | Always copy objects to UUID-derived paths. Rewrite metadata. Same-name optimization via ListObjectsV2. |
| CopyObject fallback | §5.4 | If CopyObject fails (cross-region), fall back to streaming GET → PUT. |
| Mixed disk handling | §16.2 | Per-part disk routing: local disk → compress+upload, S3 disk → CopyObject. |

### 2d — Resume & Reliability

| Component | Design section | Details |
|-----------|---------------|---------|
| Upload resume | §3.6 | `upload.state.json`: tracks completed files. `--resume` skips them. Invalidate on param change. |
| Download resume | §4 | `download.state.json`: same pattern. |
| Restore resume | §5.3 | `restore.state.json`: tracks attached parts. Query `system.parts` for already-attached on resume. |
| State degradation | §16.1 | State file write failure → warning, continue. Never fatal. |
| Post-download verify | §4 | CRC64 check after decompress. Retry on mismatch. |
| Manifest atomicity | §3.6 | Upload to `.tmp`, CopyObject to final key, delete `.tmp`. |
| Broken backup detection | §8.4 | Scan for missing/corrupt `metadata.json`. Show `[BROKEN]` in list. |
| Parts column check | §3.3 | Single batch query across all target tables. |
| Disk space pre-flight | §4 | `system.disks` free_space minus CRC64-matched hardlink savings. |
| ClickHouse TLS | §12 | `secure` and `tls_ca` are fully supported. `skip_verify` and mutual TLS (`tls_cert`/`tls_key`) parse and log but have no effect because the `clickhouse-rs` HTTP client does not expose a direct cert/key API; TLS env-var wiring (SSL_CERT_FILE etc.) is attempted but not guaranteed to work across all deployments. |
| Partition-level backup | §3.4 | `--partitions` flag: use `ALTER TABLE FREEZE PARTITION 'X'` per partition instead of whole-table FREEZE. |
| Disk filtering | §12 | `skip_disks: []` and `skip_disk_types: []` — exclude cache/temporary disks from backup. |

### Tests

Expand integration test suite.
Tests marked **(impl)** are in `test/run_tests.sh`; unmarked are aspirational future work.

- T4: Incremental backup chain **(impl: `test_incremental_chain`)**
- T5: Schema-only backup **(impl: `test_schema_only`)**
- T6: Partitioned restore **(impl: `test_partitioned_restore`)**
- T8: Backup name validation **(impl: `test_backup_name_validation`)**
- T15: Restore mode A — `--rm` DROP first **(impl: `test_restore_mode_a`)**
- T25: Partition-level create — `--partitions` **(impl: `test_partitioned_create`)**
- Replicated tables + ZK path handling — aspirational (requires multi-replica)
- Crash recovery — aspirational (requires SIGKILL mid-freeze)
- Large parts → multipart upload — aspirational
- Cross-version compatibility — CI matrix provides implicit coverage

### Definition of done

```bash
# Parallel multi-table backup
chbackup create -t "default.*" daily_full

# Incremental
chbackup create -t "default.*" --diff-from daily_full daily_incr
# Should show: "Skipped 45 parts (CRC64 match), uploaded 3 new parts"

# Upload + resume
chbackup upload daily_incr
# Kill mid-upload, then:
chbackup upload --resume daily_incr
# Should show: "Resuming: 12/15 parts already uploaded, uploading remaining 3"

# S3 disk table backup + restore with isolation
chbackup create -t default.s3_table s3_test
chbackup upload s3_test
chbackup download s3_test
chbackup restore -t default.s3_table --as=default.s3_table_copy s3_test
# Both tables work independently; DROP one doesn't affect the other
```

### Design sections consumed: §3 (complete), §4 (complete), §5.1-5.4 (complete), §7 (finalized), §8.4, §11 (complete), §16.1-16.3

---

## Phase 3 — Operations: Server, Watch, API, Retention

**Goal**: The tool runs as a long-lived process in Kubernetes. Automatic scheduled backups, HTTP API for manual operations, Prometheus metrics, retention cleanup. After Phase 3, this is a production-ready sidecar.

**Duration**: 7-10 days

### 3a — API Server

| Component | Design section | Details |
|-----------|---------------|---------|
| HTTP server | §9 | `axum` on tokio. Bind to `api.listen` address. |
| All endpoints | §9 | Every endpoint from the API table. Each delegates to the same functions as CLI commands. |
| Action log | §9 | In-memory ring buffer of recent operations with status, duration, error messages. `GET /api/v1/actions`. |
| Kill support | §9 | `CancellationToken` per operation. `POST /api/v1/kill` triggers it. |
| Health + version | §9 | `/health` returns 200. `/api/v1/version` returns binary version. |
| Integration tables | §9.1 | On startup, CREATE `system.backup_list` and `system.backup_actions` as URL engine tables pointing at the local API. Lets operators query backup status and trigger operations from `clickhouse-client`. DROP on shutdown. Config: `create_integration_tables: true` (default). |
| API authentication | §9 | Basic auth via `api.username` / `api.password`. When set, all endpoints require Authorization header. 401 on failure. |
| API TLS | §9 | `api.secure: true` with certificate/key files. Required for non-localhost deployments. |
| POST /restart | §9 | Reload config from disk, reconnect ClickHouse/S3 clients with ping gate, atomic ArcSwap. Does NOT rebind the TCP socket (axum holds the listener; socket rebind requires process restart). |
| Auto-resume on restart | §9 | `complete_resumable_after_restart: true`. On startup, scan for `*.state.json`, queue resume ops. |
| Allow parallel ops | §9 | `allow_parallel: false` (default). When true, concurrent operations on different backup names allowed. |
| Integration tables host | §9.1 | `integration_tables_host` — DNS name for URL engine tables (K8s service name). |
| Watch lifecycle | §10 | `watch_is_main_process: false`. When true, watch failure exits the server (K8s restarts it). |

```
src/
├── server/
│   ├── mod.rs
│   ├── routes.rs           # All endpoint handlers
│   ├── actions.rs          # Action log ring buffer
│   └── metrics.rs          # Prometheus registry
```

### 3b — Prometheus Metrics

| Metric | Type | Description |
|--------|------|-------------|
| `chbackup_backup_duration_seconds` | Histogram | Per-operation (create, upload, download, restore) |
| `chbackup_backup_size_bytes` | Gauge | Last backup compressed size |
| `chbackup_backup_last_success_timestamp` | Gauge | Unix timestamp |
| `chbackup_parts_uploaded_total` | Counter | Parts uploaded (vs skipped incremental) |
| `chbackup_parts_skipped_incremental_total` | Counter | Parts skipped via diff-from |
| `chbackup_errors_total` | Counter | Per operation type |
| `chbackup_number_backups_remote` | Gauge | Current count |
| `chbackup_number_backups_local` | Gauge | Current count |
| `chbackup_in_progress` | Gauge | 1 if operation running, 0 otherwise |
| `chbackup_watch_state` | Gauge | Enum: idle/full/incremental/sleeping/error |
| `chbackup_watch_last_full_timestamp` | Gauge | Unix timestamp |
| `chbackup_watch_consecutive_errors` | Gauge | Current error count |

Use `prometheus` crate. Expose via `GET /metrics`.

### 3c — Retention / GC

| Component | Design section | Details |
|-----------|---------------|---------|
| Local retention | §8.1, §8.3 | Delete oldest local backups exceeding `backups_to_keep_local`. `-1` = delete after upload. |
| Remote GC | §8.2 | Load all manifests, build referenced key set, re-check for races, batch delete orphaned keys. |
| Remote retention | §8.3 | After upload, delete oldest remote backups exceeding count. Uses GC for safe deletion. |
| Manifest caching | §8.2 | In server mode: cache manifest key-sets, incremental refresh. |
| `clean` | §13 | Walk shadow dirs matching `chbackup_*`. Check PID lock before removing. |
| `clean_broken` | §8.4 | Scan local/remote for missing metadata. Delete broken backups. |

### 3d — Watch Mode

| Component | Design section | Details |
|-----------|---------------|---------|
| State machine | §10.4 | Resume → Decide → Create → Upload → Delete local → Retention → Sleep → loop. Error path with backoff. |
| Resume on restart | §10.5 | Scan remote backups matching template, calculate elapsed since last full/incr, determine next type. |
| Name template | §10.3 | `{type}`, `{time:FORMAT}`, `{shard}` macro resolution. |
| Error → full fallback | §10.6 | After any error, next backup is always full. Consecutive error count → abort after max. |
| Config hot-reload | §10.8 | SIGHUP sets `reload_pending` flag. Apply at next sleep cycle. API: `POST /api/v1/reload`. |
| `server --watch` | §10.9 | API server + watch loop in one process. Primary K8s deployment. |

### 3e — Docker / Deployment

| Component | Design section | Details |
|-----------|---------------|---------|
| Dockerfile | §1.2 | Multi-stage: Rust builder → Alpine runtime. Static musl. |
| Integration test image | §1.4 | All-in-one: CH server + chbackup binary. |
| CI matrix | §1.4.6 | GitHub Actions: CH versions 23.8, 24.3, 24.8, 25.1. |
| K8s sidecar manifest | §1.3, §10.9 | Example YAML with `server --watch`, env vars, volume mounts. |

### Tests

Tests marked **(impl)** are in `test/run_tests.sh`; unmarked are aspirational future work.

- T7: Server API create + upload **(impl: `test_server_api_create_upload`)**
- T9: Delete and list **(impl: `test_delete_and_list`)**
- T10: Clean broken **(impl: `test_clean_broken`)**
- T19: Retention (local + remote) **(impl: `test_retention`)**
- T23: API concurrent rejection (HTTP 423) **(impl: `test_api_concurrent`)**
- T29: Watch mode — full + incremental cycle **(impl: `test_watch_mode`)**
- T53: API basic auth **(impl: `test_api_auth`)**
- Watch resume after restart — aspirational
- Watch error recovery — aspirational
- Auto-resume after server restart — aspirational

### Definition of done

```bash
# Server + watch mode running
chbackup server --watch &

# API works
curl http://localhost:7171/api/v1/list?location=remote
curl http://localhost:7171/api/v1/watch/status
# {"state":"sleeping","last_full":"2025-02-15T02:00:00Z","last_incr":"2025-02-15T03:00:00Z","next_in":"47m"}

# Integration tables — query backups from clickhouse-client
clickhouse-client -q "SELECT name, created, formatReadableSize(size) FROM system.backup_list ORDER BY created"
clickhouse-client -q "INSERT INTO system.backup_actions(command) VALUES ('create_remote test_backup')"
clickhouse-client -q "SELECT * FROM system.backup_actions WHERE status = 'in progress'"

# Prometheus metrics available
curl http://localhost:7171/metrics | grep chbackup_watch_state

# Retention works
curl -X POST http://localhost:7171/api/v1/clean/remote_broken

# SIGHUP reloads config
kill -HUP $(pidof chbackup)
# Logs: "Config reloaded: watch_interval=1h→30m"
```

### Design sections consumed: §1 (complete), §8 (complete), §9 (complete), §10 (complete), §13

---

## Phase 4 — Polish: Remap, Dependencies, Edge Cases

**Goal**: Everything else. These features matter for advanced use cases but aren't blocking production deployment. Each item is independent — build in any order based on demand.

**Duration**: 5-8 days (or ongoing)

### 4a — Table / Database Remap

| Component | Design section | Priority |
|-----------|---------------|----------|
| `--as=dst_db.dst_table` | §6.1 | High — common restore scenario |
| DDL rewriting | §6.1 | Table name, UUID, ZK path, Distributed references |
| `-m, --database-mapping` | §6.2 | Medium — prod→staging restores |
| `restore_remote` | §2 | Medium — download+restore in one step |

### 4b — Dependency-Aware Restore

| Component | Design section | Priority |
|-----------|---------------|----------|
| Topological sort | §5.5 | High — correctness for views/dictionaries |
| Dependency graph in manifest | §5.5, §7 | Store `dependencies_database`/`dependencies_table` from `system.tables` |
| Phase 3 DDL restore | §5.5 | Dictionaries, Views, MVs in topo order |
| Phase 4 restore | §5.6 | Functions, Named Collections, RBAC file restore + chown |
| Fallback retry loop | §5.5 | For CH < 23.3 without dependency info |

### 4c — Streaming Engine Postponement

| Component | Design section | Priority |
|-----------|---------------|----------|
| Phase 2b table classification | §5.1 | Detect Kafka/NATS/RabbitMQ/S3Queue engines |
| Postponed CREATE | §5.1, §5.5 | Create streaming tables AFTER all data attached |
| Refreshable MV detection | §5.1 | MVs with `REFRESH` clause |

### 4d — Advanced Restore Modes

| Component | Design section | Priority |
|-----------|---------------|----------|
| Mode A full restore | §5.2 | `--rm` DROP + CREATE + restore. Reverse engine priority for DROP. |
| ATTACH TABLE mode | §5.2 | `restore_as_attach: true`. DETACH → DROP REPLICA → ATTACH → RESTORE REPLICA. |
| ZK path conflict resolution | §5.2 | Parse Replicated params, resolve macros, check system.zookeeper, DROP REPLICA with actual path. |
| `--schema` flag | §2 | Schema-only backup/restore (no data) |
| `--data-only` flag | §2 | Data-only restore (no schema) |
| Pending mutation re-apply | §5.7 | Re-apply mutations from manifest with warnings |
| ON CLUSTER restore | §5.1 | `restore_schema_on_cluster` — execute DDL with ON CLUSTER clause for cluster-wide restore |
| Replicated Database Engine | §5.1 | Detect DatabaseReplicated — skip ON CLUSTER (implicit), regenerate UUIDs |
| Distributed table cluster fix | §5.1 | `restore_distributed_cluster` — rewrite Distributed engine cluster references |

### 4e — RBAC & Config Backup/Restore

| Component | Design section | Priority |
|-----------|---------------|----------|
| `--rbac` / `--rbac-only` flags | §2, §3.4 | Backup: serialize users/roles/quotas from system tables to `access/` dir |
| `--configs` / `--configs-only` flags | §2, §3.4 | Backup: copy CH config files from `config_dir` to backup |
| `rbac_backup_always` | §12 | Always include RBAC in every backup without needing the flag |
| `rbac_resolve_conflicts` | §12 | On restore: "recreate" (drop+create), "ignore" (skip), "fail" (error) |
| Named collections backup | §5.6 | `named_collections_backup_always` — query system.named_collections |
| CH restart after RBAC restore | §5.6 | `restart_command` — execute after RBAC/config restore to apply changes |

### 4f — Operational Extras

| Component | Design section | Priority |
|-----------|---------------|----------|
| `tables` command | §2 | List tables from CH or from remote backup |
| JSON/Object column detection | §16.4 | Warn on unfreeeable column types |
| Part sizes in `list` output | §7 | Human-readable sizes from manifest |
| Additional compression formats | §12 | Add `gzip` and `none` beyond lz4/zstd |

### Tests

Test coverage for Phase 4 features in `test/run_tests.sh` (62 tests total, T4-T62 + smoke/round-trip):

- [done] T28: RBAC backup + restore round-trip with `--rbac` flag
- [done] T6/T25: Partition-level backup/restore with `--partitions`
- [done] T12: Incremental backup/restore with S3 disk tables
- [done] T56: Configs backup + restore with `--configs` flag
- [done] Replicated tables + ZK path conflict (`resolve_zk_conflict()` in restore/schema.rs)
- [deferred] Streaming engine activation test (requires Kafka/NATS broker in CI — infrastructure change out of scope)
- [done] ON CLUSTER restore (`add_on_cluster_clause()` in restore/schema.rs)
- [done] DatabaseReplicated engine (`query_database_engine()` in clickhouse/client.rs)

### Design sections consumed: §5.5-5.7, §6, §16.4

---

## Milestone Summary

```
Phase 0 (3-5 days)     Phase 1 (7-10 days)      Phase 2 (10-14 days)     Phase 3 (7-10 days)     Phase 4 (ongoing)
──────────────────     ────────────────────      ────────────────────     ───────────────────     ─────────────────
CLI + config           Single-table E2E         Parallel + incremental   Server + watch          Remap + deps
CH connection          FREEZE/UNFREEZE          Flat semaphores          HTTP API + auth         Topo sort
S3 connection          Streaming upload         CRC64 diff-from          Prometheus              Streaming engines
PID lock               Streaming download       S3 object disk           Retention/GC            Advanced restore
default-config         Hardlink restore         Multipart upload         Docker/CI               RBAC/Config backup
print-config           Manifest V1              Resume all ops           SIGHUP reload           ON CLUSTER restore
                       list, delete             Broken detection         Integration tables      Replicated DB Engine
                       SortPartsByMinBlock      CH TLS connection        API TLS + restart       Schema/data-only
                       ignore_not_exists        Partition-level freeze   Auto-resume on restart
                       allow_empty_backups      Disk space check         allow_parallel
                       log_sql_queries          skip_disks/skip_types    watch_is_main_process

Compiles,              Backup & restore         Production-ready         Deployable              Feature-complete
connects               works (1 table)          (multi-table, fast)      K8s sidecar             parity + beyond
```

## Design Section → Phase Mapping

| Design Section | Phase | Notes |
|---|---|---|
| §1 Deployment | 3e | Dockerfile, K8s manifest, CI |
| §2 Commands | 0 (skeleton) + 1-4 (implementation) | CLI defined in P0, each command implemented when its logic is ready |
| §3 Backup Flow | 1 (sequential) → 2a (parallel) → 2b (incremental) → 2c (S3 disk) | Core pipeline built incrementally |
| §4 Download | 1 (sequential) → 2a (parallel) → 2d (resume + verify) | Same pattern as upload |
| §5 Restore Flow | 1 (Mode B, local) → 2a (parallel) → 2c (S3 disk) → 4b-4d (advanced) | Simplest mode first |
| §6 Table Remap | 4a | Not blocking production use |
| §7 Manifest Format | 1 (struct definition) | Defined once, extended as needed |
| §8 Retention / GC | 3c | Needs server mode context for caching |
| §9 API Server | 3a-3b | Depends on all commands existing |
| §10 Watch Mode | 3d | Depends on API + retention |
| §11 Async Architecture | 2a | Semaphore model applied to all operations |
| §12 Configuration | 0 | Foundation |
| §13 Clean Command | 3c | Simple, bundle with retention |
| §14 Feature Comparison | — | Marketing, not code |
| §15 Known Issues | 1-4 | Each fix lands with its feature |
| §16 Implementation Notes | 2d (state degradation), 2c (tiered storage), 2a (space check), 4e (JSON detect) | Cross-cutting |
| §17 Deferred | v2 | Out of scope |

## Crate Dependencies (lock these in Phase 0)

```toml
[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# CLI
clap = { version = "4", features = ["derive"] }

# Config
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"
serde_json = "1"

# ClickHouse
clickhouse = { version = "0.13", features = ["inserter"] }  # async CH driver (HTTP)

# S3
aws-sdk-s3 = "1"
aws-config = "1"

# Compression (sync in Phase 1, streaming in Phase 2)
lz4_flex = "0.11"             # pure Rust lz4 (sync, via spawn_blocking)
zstd = "0.13"
tokio-util = { version = "0.7", features = ["codec"] }

# Archive (sync, via spawn_blocking)
tar = "0.4"

# Checksums
crc = "3"                     # CRC64 (CRC_64_XZ algorithm) for checksums.txt verification

# Error handling
thiserror = "2"
anyhow = "1"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["json", "env-filter"] }

# HTTP server (Phase 3)
axum = "0.7"
prometheus = "0.13"

# Filesystem
walkdir = "2"
nix = { version = "0.29", features = ["fs", "user"] }  # chown, stat

# Misc
chrono = { version = "0.4", features = ["serde"] }
glob = "0.3"                  # table pattern matching
```

## Risks & Mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Sync `tar` + `spawn_blocking` overhead | Slight overhead from thread pool dispatch | Negligible for Phase 1 (buffered). Phase 2 streaming can use `SyncIoBridge` + duplex pipe if needed. |
| `clickhouse` crate missing features | Can't execute some CH commands | Crate uses HTTP protocol, covers DDL/DML. Fallback: raw HTTP request for edge cases. |
| S3 multipart streaming without knowing size | Can't use PutObject for small parts | Decision threshold at 32MB uncompressed. Small parts use buffered PutObject (acceptable — they're small). |
| CRC64 computation performance on large parts | Slow backup of tables with huge parts | CRC64 is ~3GB/s on modern CPUs. 10GB part = 3 seconds. Not a bottleneck vs S3 upload time. |
| Object disk metadata v5 (CH 25.10+) | New format not yet widely deployed | Implement parser from spec, test against CH 25.1 nightly. Can defer to Phase 4 if no users need it yet. |
