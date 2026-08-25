# Configuration Reference

chbackup reads configuration from a YAML file, environment variables, and CLI flags. This page documents all parameters.

## Table of contents

- [Config file location](#config-file-location)
- [Priority order](#priority-order)
- [Minimal config](#minimal-config)
- [General](#general)
- [ClickHouse](#clickhouse)
- [S3](#s3)
- [Backup](#backup)
- [Retention](#retention)
- [Watch](#watch)
- [API](#api)
- [All environment variables](#all-environment-variables)
- [CLI --env overrides](#cli-env-overrides)

## Config file location

chbackup resolves the config file using this fallback chain:

1. `-c` / `--config` CLI flag (error if file missing)
2. `CHBACKUP_CONFIG` environment variable
3. `CLICKHOUSE_BACKUP_CONFIG` environment variable (Go clickhouse-backup compatibility)
4. `/etc/chbackup/config.yml` (if exists)
5. `/etc/clickhouse-backup/config.yml` (Go clickhouse-backup compatibility, if exists)
6. Default: `/etc/chbackup/config.yml` (empty config with defaults)

```bash
# CLI flag (highest priority)
chbackup -c /path/to/config.yml create

# Environment variable
CHBACKUP_CONFIG=/path/to/config.yml chbackup create

# Go clickhouse-backup compatible env var
CLICKHOUSE_BACKUP_CONFIG=/etc/clickhouse-backup/config.yml chbackup create
```

Generate a config file with all defaults:

```bash
chbackup default-config > /etc/chbackup/config.yml
```

## Priority order

Values are resolved from highest to lowest priority:

1. **CLI `--env` flags** -- `chbackup --env s3.bucket=other create`
2. **Environment variables** -- `S3_BUCKET=other chbackup create`
3. **Config file** -- values from the YAML file
4. **Built-in defaults** -- hardcoded defaults

## Minimal config

The smallest config needed to run chbackup (assuming S3 credentials via env vars or IAM):

```yaml
s3:
  bucket: my-backup-bucket
  region: us-east-1
```

Everything else uses sensible defaults: ClickHouse at `localhost:8123`, data at `/var/lib/clickhouse`, LZ4 compression, 4 concurrent workers.

## General

Controls logging, concurrency, retry behavior, and progress tracking. These apply globally to all commands.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `log_level` | string | `info` | Log level: `debug`, `info`, `warning`, `error` |
| `log_format` | string | `text` | Log format: `text` (human-readable) or `json` (structured) |
| `disable_progress_bar` | bool | `false` | Disable progress bar (auto-disabled when stdout is not a TTY) |
| `backups_to_keep_local` | int | `0` | Local retention count. 0 = unlimited, -1 = delete after upload |
| `backups_to_keep_remote` | int | `7` | Remote retention count. 0 = unlimited |
| `upload_concurrency` | int | `4` | Number of parallel part uploads |
| `download_concurrency` | int | `4` | Number of parallel part downloads |
| `upload_max_bytes_per_second` | int | `0` | Upload rate limit (bytes/sec). 0 = unlimited |
| `download_max_bytes_per_second` | int | `0` | Download rate limit (bytes/sec). 0 = unlimited |
| `object_disk_server_side_copy_concurrency` | int | `32` | Parallel CopyObject calls for S3 disk parts |
| `retries_on_failure` | int | `3` | Retry count for transient failures |
| `retries_pause` | string | `5s` | Wait between retries |
| `retries_jitter` | int | `30` | Percent jitter on retry pause (0-100) |
| `use_resumable_state` | bool | `true` | Track progress in state files for `--resume` |
| `remote_cache_ttl_secs` | int | `300` | TTL for in-memory remote manifest cache (server mode). 0 = disabled |
| `clean_broken_min_age_secs` | int | `3600` | Minimum age of a broken remote backup's newest object before `clean_broken` deletes it. 0 = delete regardless of age |

Upload writes `metadata.json` **last**, so a backup still being uploaded is
indistinguishable from a broken one. `clean_broken remote` therefore skips a broken backup
when its PID lock is held by a live process, when its newest object was written less than
`clean_broken_min_age_secs` ago (or carries no timestamp at all), and never deletes a key
that a surviving backup's manifest still references. Every skip is logged with its reason.
Lower `clean_broken_min_age_secs` only if no upload can outlive the window.

## ClickHouse

Connection settings and backup/restore behavior. chbackup must run on the same host as ClickHouse.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `host` | string | `localhost` | ClickHouse hostname |
| `port` | int | `8123` | ClickHouse HTTP port |
| `username` | string | `default` | ClickHouse username |
| `password` | string | _(empty)_ | ClickHouse password |
| `data_path` | string | `/var/lib/clickhouse` | Path to ClickHouse data directory |
| `config_dir` | string | `/etc/clickhouse-server` | Path to ClickHouse server config directory |
| `secure` | bool | `false` | Use TLS for ClickHouse connection |
| `skip_verify` | bool | `false` | Skip TLS certificate verification |
| `tls_key` | string | _(empty)_ | TLS client key file path |
| `tls_cert` | string | _(empty)_ | TLS client certificate file path |
| `tls_ca` | string | _(empty)_ | TLS custom CA file path |
| `sync_replicated_tables` | bool | `true` | Run SYSTEM SYNC REPLICA before FREEZE |
| `check_replicas_before_attach` | bool | `true` | Verify replicas are synced before ATTACH |
| `strict_replica_sync` | bool | `true` | With the default, when `check_replicas_before_attach` polling **completes and reports the replica is still behind**, that table is **not attached** and all of its parts are reported as skipped, so the restore exits 3 — matching Go's `CheckReplicationInProgress`, which returns before attaching. The rationale is partial restore, not row duplication: a table brought live while the other replica's parts are still arriving is incomplete and inconsistent for readers, and a loud exit 3 is better than a table that silently looks restored. (Attaching a backup's parts on a second replica does **not** duplicate rows — ClickHouse derives the `/blocks` deduplication key from part content, not from the block number assigned at ATTACH time, identically in 23.8–25.1. A residual, *unconfirmed* path exists via `/blocks` window eviction when a restore attaches more parts than `replicated_deduplication_window`, but that is a different mechanism and is not what this flag is justified by.) Set to `false` to restore the old behaviour — warn and attach anyway. A sync check that **fails** (e.g. `system.replicas` unreachable) is indeterminate and always warns and proceeds, in both modes |
| `check_parts_columns` | bool | `false` | Validate column type consistency before backup |
| `mutation_wait_timeout` | string | `5m` | Timeout for waiting on mutations to complete |
| `restore_as_attach` | bool | `false` | Use DETACH/ATTACH TABLE mode for full restores |
| `attach_table_require_complete` | bool | `true` | With the default, restore **refuses to attach a Replicated table whose parts are not all present**: the table is not created, not attached, and all of its parts are reported as skipped, so the restore exits 3. Set to `false` to attach whatever parts are staged and count the deficit as skipped — that brings the table live with known-missing data. Only affects `restore_as_attach` mode |
| `zk_check_strict` | bool | `true` | With the default, when `system.zookeeper` cannot be queried for a Replicated table, that table is **not created** and the restore fails for it: an unreachable ZooKeeper is not evidence that no stale replica is registered, and creating the table anyway is exactly the case ZK conflict resolution exists to catch. Set to `false` to restore the old permissive behaviour — the table is created without conflict resolution, with a warning saying the check was skipped (a stale replica, if any, is left in place, so CREATE may fail with an existing-replica error). Verified-clean ZooKeeper (proceed) and a found conflict (`SYSTEM DROP REPLICA`) are unaffected either way |
| `replace_uuid_macro` | bool | `false` | Substitute the backed-up table UUID for a literal `{uuid}` in a Replicated engine's ZooKeeper path before CREATE, matching Go's `replaceUUIDMacroValue` (Altinity #466). Off by default because rewriting a Replicated table's ZooKeeper path is itself a behaviour change, so the conservative choice is to keep passing the manifest DDL through unchanged. Enable it if a restore fails with error 36 (`BAD_ARGUMENTS`) complaining that the `uuid` macro is "supported only for ON CLUSTER queries with Atomic database engine": ClickHouse 23.8–25.1 only expands `{uuid}` for `ON CLUSTER` queries, DatabaseReplicated databases, `ATTACH`, or a DDL carrying an explicit UUID clause, and otherwise refuses the CREATE outright. The stronger claim that each node instead expands `{uuid}` to a different freshly generated UUID and so forms separate replication groups **could not be confirmed** — tracing the 23.8–25.1 sources found the opposite, a hard CREATE error; see `docs/verification/h3-h4-replicated-semantics.json`. Only the ZooKeeper-path argument is rewritten; a `{uuid}` elsewhere in the DDL is left for ClickHouse to interpret |
| `restore_schema_on_cluster` | string | _(empty)_ | Execute DDL with ON CLUSTER clause. Set to cluster name |
| `restore_distributed_cluster` | string | _(empty)_ | Rewrite Distributed engine cluster name during restore |
| `max_connections` | int | `1` | Concurrent restore table operations |
| `log_sql_queries` | bool | `true` | Log SQL queries at info level |
| `ignore_not_exists_error_during_freeze` | bool | `true` | Skip tables dropped during backup |
| `freeze_by_part` | bool | `false` | Freeze individual partitions instead of whole table. IDs are discovered from `system.parts.partition_id` and issued as `FREEZE PARTITION ID` |
| `freeze_by_part_where` | string | _(empty)_ | Additional WHERE filter on `system.parts` for partition selection |
| `deferred_freeze_ttl_secs` | int | `86400` (24h) | How long an *orphaned* S3 object-disk freeze is held before it may be reaped. Tables with object-disk parts stay FROZEN between `create` and `upload` so their remote objects cannot be garbage-collected before CopyObject reads them. **This value must exceed your worst-case upload duration** — nothing in the binary can verify that, since the duration is not knowable in advance, so it is on you to check. Minimum accepted is `3600`, which is only a sanity floor and is *well below* a realistic multi-TiB object-disk upload (over 4h observed in production). Setting it too low releases a freeze a retrying upload still needs, surfacing as CopyObject `NoSuchKey`. If an upload ever outlives the TTL, chbackup logs a `warn!` naming the elapsed time and the TTL — alert on it |
| `backup_mutations` | bool | `true` | Backup pending mutations from `system.mutations` |
| `restart_command` | string | `exec:systemctl restart clickhouse-server` | Command after RBAC/config restore |
| `debug` | bool | `false` | Verbose ClickHouse client debug logging |
| `rbac_backup_always` | bool | `false` | Always include RBAC in backups (no `--rbac` flag needed) |
| `config_backup_always` | bool | `false` | Always include configs in backups |
| `named_collections_backup_always` | bool | `false` | Always include named collections |
| `rbac_resolve_conflicts` | string | `recreate` | RBAC conflict strategy: `recreate`, `ignore`, `fail` |
| `skip_tables` | list | `[system.*, INFORMATION_SCHEMA.*, information_schema.*, _temporary_and_external_tables.*]` | Table patterns to exclude from backup |
| `skip_table_engines` | list | `[]` | Engine names to exclude (e.g., `Kafka`, `S3Queue`) |
| `skip_disks` | list | `[]` | Disk names to exclude |
| `skip_disk_types` | list | `[]` | Disk types to exclude (e.g., `cache`, `local`) |
| `default_replica_path` | string | `/clickhouse/tables/{shard}/{database}/{table}` | Default ZooKeeper replica path |
| `default_replica_name` | string | `{replica}` | Default replica name |
| `timeout` | string | `5m` | ClickHouse query timeout |

## S3

S3 storage settings. See the [S3 guide](s3.md) for provider-specific setup.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `bucket` | string | `my-backup-bucket` | S3 bucket name |
| `region` | string | `us-east-1` | AWS region |
| `endpoint` | string | _(empty)_ | Custom endpoint for MinIO, R2, etc. |
| `prefix` | string | `chbackup` | Key prefix. Supports `{shard}` macro |
| `access_key` | string | _(empty)_ | AWS access key ID |
| `secret_key` | string | _(empty)_ | AWS secret access key |
| `assume_role_arn` | string | _(empty)_ | IAM role ARN for cross-account access |
| `force_path_style` | bool | `false` | Path-style addressing (required for MinIO, Ceph) |
| `disable_ssl` | bool | `false` | Disable HTTPS |
| `disable_cert_verification` | bool | `false` | Skip TLS certificate verification |
| `acl` | string | _(empty)_ | Canned ACL: `private`, `bucket-owner-full-control` |
| `storage_class` | string | `STANDARD` | S3 storage class |
| `sse` | string | _(empty)_ | Encryption: `AES256` or `aws:kms` |
| `sse_kms_key_id` | string | _(empty)_ | KMS key ID for `aws:kms` encryption |
| `max_parts_count` | int | `10000` | Max parts per multipart upload |
| `chunk_size` | int | `0` | Multipart chunk size (0 = auto) |
| `concurrency` | int | `1` | SDK internal concurrency per upload |
| `object_disk_path` | string | _(empty)_ | Alternate prefix for S3 disk objects |
| `allow_object_disk_streaming` | bool | `false` | Fallback to streaming download+reupload when CopyObject fails. Does **not** help when the source object is missing — streaming GETs the same key |
| `debug` | bool | `false` | Verbose S3 SDK logging |

## Backup

Controls compression, concurrency, and retry behavior specifically for backup data operations. These override `general.*` values when non-zero.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `tables` | string | `*.*` | Default table filter pattern |
| `allow_empty_backups` | bool | `false` | Allow creating backup with no matching tables |
| `compression` | string | `lz4` | Algorithm: `lz4`, `zstd`, `gzip`, `none` |
| `compression_level` | int | `1` | Compression level (algorithm-dependent) |
| `upload_concurrency` | int | `4` | Override `general.upload_concurrency` for backups |
| `download_concurrency` | int | `4` | Override `general.download_concurrency` for backups |
| `object_disk_copy_concurrency` | int | `8` | Concurrent S3 CopyObject for object disk parts |
| `upload_max_bytes_per_second` | int | `0` | Override upload rate limit |
| `download_max_bytes_per_second` | int | `0` | Override download rate limit |
| `retries_on_failure` | int | `5` | Override retry count |
| `retries_duration` | string | _(empty)_ | Override retry wait (empty = use `general.retries_pause`) |
| `retries_jitter` | float | `0.1` | Retry jitter fraction (0.0-1.0) |
| `skip_projections` | list | `[]` | Projection patterns to skip |
| `strict_projection_gate` | bool | `true` | Refuse to restore a backup that had projections stripped onto ClickHouse older than 24.3 |
| `streaming_upload_threshold` | int | `268435456` | Parts larger than this (bytes, default 256 MiB) use streaming multipart upload |

### Stripped projections and `strict_projection_gate`

`skip_projections` omits matching `.proj` directories from the staged parts, but the
part's `checksums.txt` is hardlinked verbatim and still lists them. Backups therefore
record the projections that were **actually removed** in the manifest's
`stripped_projections` field — a sorted, deduplicated list of projection names. A
pattern that matched nothing on disk records an empty list, so the field describes the
backup rather than the configuration.

`strict_projection_gate` (default `true`) makes `restore` refuse such a backup when the
target server is older than ClickHouse 24.3. Before 24.3 the missing directory fails an
existence check while the parent part's checksums are validated, and ATTACH rejects the
whole part with `Code: 107. FILE_DOESNT_EXIST`; 24.3 skips that consistency pass once a
projection is known broken and attaches the part instead. This is traced in
`docs/verification/h9-projection-gate.json` (verdict CONFIRMED) and is also the
threshold upstream clickhouse-backup uses. Set it to `false` to attempt the restore
anyway.

Backups written before this field existed have no `stripped_projections` at all. That
absence means "nothing recorded", never "projections were stripped", so they restore
exactly as they did before in either flag state.

## Retention

Retention counts applied after backup and upload operations. These override `general.backups_to_keep_*` when non-zero.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `backups_to_keep_local` | int | `0` | Local retention count. 0 = use `general.*` value |
| `backups_to_keep_remote` | int | `0` | Remote retention count. 0 = use `general.*` value |

### How retention works

After each upload, chbackup:

1. Lists all local/remote backups (excluding broken ones)
2. Sorts by creation time (oldest first)
3. Deletes the oldest backups exceeding the keep count

Special values:

- `0` -- unlimited (no retention)
- `-1` (local only) -- delete local backup immediately after successful upload

Remote deletion uses garbage collection: only S3 keys not referenced by surviving backups are removed. Incremental base backups are protected from deletion as long as any backup references them.

For a detailed explanation of the retention flow, GC mechanics, and override priority, see [Backup Guide > Retention](backup.md#retention).

## Watch

Scheduled backup loop settings. Activated via `chbackup watch` or `chbackup server --watch`.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `enabled` | bool | `false` | Enable watch loop in server mode |
| `watch_interval` | string | `1h` | Time between backup checks |
| `full_interval` | string | `24h` | Time between full backups |
| `name_template` | string | `shard{shard}-{type}-{time:%Y%m%d_%H%M%S}` | Backup name template |
| `max_consecutive_errors` | int | `5` | Abort after N consecutive failures. 0 = unlimited |
| `retry_interval` | string | `5m` | Wait before retrying after error |
| `delete_local_after_upload` | bool | `true` | Delete local backup after successful upload |
| `tables` | string | _(empty)_ | Table filter pattern for watch backups (empty = use `backup.tables`) |

### Name template macros

| Macro | Description | Example output |
|-------|-------------|----------------|
| `{type}` | Backup type | `full` or `incremental` |
| `{time:FORMAT}` | Timestamp (chrono format) | `20240115_120000` |
| `{shard}` | From ClickHouse `system.macros` | `01` |

## API

HTTP server settings. Activated via `chbackup server`.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `listen` | string | `localhost:7171` | Listen address |
| `enable_metrics` | bool | `true` | Enable Prometheus `/metrics` endpoint |
| `create_integration_tables` | bool | `true` | Create `system.backup_list` and `system.backup_actions` URL tables |
| `integration_tables_host` | string | _(empty)_ | DNS name for URL engine (default: `localhost`) |
| `username` | string | _(empty)_ | Basic auth username (empty = no auth) |
| `password` | string | _(empty)_ | Basic auth password |
| `secure` | bool | `false` | Enable TLS for API |
| `certificate_file` | string | _(empty)_ | TLS certificate file path |
| `private_key_file` | string | _(empty)_ | TLS private key file path |
| `ca_cert_file` | string | _(empty)_ | TLS CA certificate file path |
| `allow_parallel` | bool | `false` | Allow concurrent operations on different backups |
| `complete_resumable_after_restart` | bool | `true` | Auto-resume interrupted operations on server startup |
| `watch_is_main_process` | bool | `false` | Exit server if watch loop dies |

## All environment variables

The most common config parameters can be overridden via environment variables. Organized by section:

### General

| Variable | Config field |
|----------|-------------|
| `CHBACKUP_LOG_LEVEL` | `general.log_level` |
| `CHBACKUP_LOG_FORMAT` | `general.log_format` |
| `CHBACKUP_BACKUPS_TO_KEEP_LOCAL` | `general.backups_to_keep_local` |
| `CHBACKUP_BACKUPS_TO_KEEP_REMOTE` | `general.backups_to_keep_remote` |
| `CHBACKUP_UPLOAD_CONCURRENCY` | `general.upload_concurrency` |
| `CHBACKUP_DOWNLOAD_CONCURRENCY` | `general.download_concurrency` |
| `CHBACKUP_RETRIES_ON_FAILURE` | `general.retries_on_failure` |
| `CHBACKUP_RETRIES_PAUSE` | `general.retries_pause` |
| `CHBACKUP_REMOTE_CACHE_TTL_SECS` | `general.remote_cache_ttl_secs` |
| `CHBACKUP_CLEAN_BROKEN_MIN_AGE_SECS` | `general.clean_broken_min_age_secs` |

### ClickHouse

| Variable | Config field |
|----------|-------------|
| `CLICKHOUSE_HOST` | `clickhouse.host` |
| `CLICKHOUSE_PORT` | `clickhouse.port` |
| `CLICKHOUSE_USERNAME` | `clickhouse.username` |
| `CLICKHOUSE_PASSWORD` | `clickhouse.password` |
| `CLICKHOUSE_DATA_PATH` | `clickhouse.data_path` |
| `CLICKHOUSE_CONFIG_DIR` | `clickhouse.config_dir` |
| `CLICKHOUSE_SECURE` | `clickhouse.secure` |
| `CLICKHOUSE_SKIP_VERIFY` | `clickhouse.skip_verify` |
| `CLICKHOUSE_TLS_KEY` | `clickhouse.tls_key` |
| `CLICKHOUSE_TLS_CERT` | `clickhouse.tls_cert` |
| `CLICKHOUSE_TLS_CA` | `clickhouse.tls_ca` |
| `CLICKHOUSE_SYNC_REPLICATED_TABLES` | `clickhouse.sync_replicated_tables` |
| `CLICKHOUSE_STRICT_REPLICA_SYNC` | `clickhouse.strict_replica_sync` (default `true` skips a table's attach when replica-sync polling completes and reports the replica is still behind, matching Go's `CheckReplicationInProgress`; set `false` for the old behaviour of warning and attaching anyway) |
| `CLICKHOUSE_ATTACH_TABLE_REQUIRE_COMPLETE` | `clickhouse.attach_table_require_complete` (default `true` refuses to attach a Replicated table whose parts are not all present) |
| `CLICKHOUSE_ZK_CHECK_STRICT` | `clickhouse.zk_check_strict` (default `true` fails a Replicated table whose ZooKeeper state could not be verified; set `false` for the old permissive behaviour of creating it without conflict resolution) |
| `CLICKHOUSE_REPLACE_UUID_MACRO` | `clickhouse.replace_uuid_macro` (default `false`; set `true` to substitute the backed-up table UUID for a literal `{uuid}` in a Replicated ZooKeeper path, for restores that fail with error 36 `BAD_ARGUMENTS` on the `uuid` macro — the per-node UUID divergence mechanism could not be confirmed, see `docs/verification/h3-h4-replicated-semantics.json`) |
| `CLICKHOUSE_MAX_CONNECTIONS` | `clickhouse.max_connections` |
| `CLICKHOUSE_TIMEOUT` | `clickhouse.timeout` |
| `CLICKHOUSE_DEBUG` | `clickhouse.debug` |
| `CLICKHOUSE_SKIP_TABLES` | `clickhouse.skip_tables` (comma-separated) |
| `CLICKHOUSE_SKIP_DISKS` | `clickhouse.skip_disks` (comma-separated) |
| `CLICKHOUSE_SKIP_DISK_TYPES` | `clickhouse.skip_disk_types` (comma-separated) |
| `CLICKHOUSE_SKIP_TABLE_ENGINES` | `clickhouse.skip_table_engines` (comma-separated) |

### S3

| Variable | Config field |
|----------|-------------|
| `S3_BUCKET` | `s3.bucket` |
| `S3_REGION` | `s3.region` |
| `S3_ENDPOINT` | `s3.endpoint` |
| `S3_PREFIX` | `s3.prefix` |
| `S3_ACCESS_KEY` | `s3.access_key` |
| `S3_SECRET_KEY` | `s3.secret_key` |
| `S3_ASSUME_ROLE_ARN` | `s3.assume_role_arn` |
| `S3_FORCE_PATH_STYLE` | `s3.force_path_style` |
| `S3_ACL` | `s3.acl` |
| `S3_STORAGE_CLASS` | `s3.storage_class` |
| `S3_SSE` | `s3.sse` |
| `S3_SSE_KMS_KEY_ID` | `s3.sse_kms_key_id` |
| `S3_DISABLE_SSL` | `s3.disable_ssl` |
| `S3_DISABLE_CERT_VERIFICATION` | `s3.disable_cert_verification` |
| `S3_CONCURRENCY` | `s3.concurrency` |
| `S3_OBJECT_DISK_PATH` | `s3.object_disk_path` |
| `S3_ALLOW_OBJECT_DISK_STREAMING` | `s3.allow_object_disk_streaming` |

### Backup

| Variable | Config field |
|----------|-------------|
| `CHBACKUP_BACKUP_COMPRESSION` | `backup.compression` |
| `CHBACKUP_BACKUP_UPLOAD_CONCURRENCY` | `backup.upload_concurrency` |
| `CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY` | `backup.download_concurrency` |
| `CHBACKUP_BACKUP_RETRIES_ON_FAILURE` | `backup.retries_on_failure` |
| `CHBACKUP_BACKUP_RETRIES_DURATION` | `backup.retries_duration` |
| `CHBACKUP_BACKUP_TABLES` | `backup.tables` |
| `CHBACKUP_BACKUP_SKIP_PROJECTIONS` | `backup.skip_projections` (comma-separated) |
| `CHBACKUP_BACKUP_STRICT_PROJECTION_GATE` | `backup.strict_projection_gate` |
| `CHBACKUP_BACKUP_STREAMING_UPLOAD_THRESHOLD` | `backup.streaming_upload_threshold` |

### API

| Variable | Config field |
|----------|-------------|
| `API_LISTEN` | `api.listen` |
| `API_SECURE` | `api.secure` |
| `API_USERNAME` | `api.username` |
| `API_PASSWORD` | `api.password` |
| `API_CREATE_INTEGRATION_TABLES` | `api.create_integration_tables` |

### Watch

| Variable | Config field |
|----------|-------------|
| `WATCH_INTERVAL` | `watch.watch_interval` |
| `FULL_INTERVAL` | `watch.full_interval` |
| `WATCH_ENABLED` | `watch.enabled` |
| `WATCH_MAX_CONSECUTIVE_ERRORS` | `watch.max_consecutive_errors` |

## Go clickhouse-backup compatibility

chbackup accepts Go clickhouse-backup environment variable names as fallbacks. The chbackup-native name always takes precedence.

| Go env var | chbackup env var | Config field |
|---|---|---|
| `LOG_LEVEL` | `CHBACKUP_LOG_LEVEL` | `general.log_level` |
| `BACKUPS_TO_KEEP_LOCAL` | `CHBACKUP_BACKUPS_TO_KEEP_LOCAL` | `general.backups_to_keep_local` |
| `BACKUPS_TO_KEEP_REMOTE` | `CHBACKUP_BACKUPS_TO_KEEP_REMOTE` | `general.backups_to_keep_remote` |
| `S3_PATH` | `S3_PREFIX` | `s3.prefix` |
| `S3_UPLOAD_CONCURRENCY` | `CHBACKUP_BACKUP_UPLOAD_CONCURRENCY` | `backup.upload_concurrency` |
| `S3_DOWNLOAD_CONCURRENCY` | `CHBACKUP_BACKUP_DOWNLOAD_CONCURRENCY` | `backup.download_concurrency` |
| `CLICKHOUSE_FREEZE_BY_PART` | — | `clickhouse.freeze_by_part` |
| `ALLOW_EMPTY_BACKUPS` | — | `backup.allow_empty_backups` |

### Port remap

Go clickhouse-backup uses the native TCP protocol (port 9000) while chbackup uses the HTTP protocol (port 8123). When `CLICKHOUSE_PORT=9000` is detected, chbackup automatically remaps it to 8123 with a warning. Set `CLICKHOUSE_PORT=8123` explicitly to suppress the warning.

### REMOTE_STORAGE

The `REMOTE_STORAGE` env var is accepted for compatibility. If set to `"s3"` or empty, it's a no-op. Any other value logs a warning since chbackup only supports S3 storage.

### Config path fallback

See [Config file location](#config-file-location) above for the `CLICKHOUSE_BACKUP_CONFIG` env var and `/etc/clickhouse-backup/config.yml` path fallbacks.

## CLI --env overrides

Override any config value from the command line using dot notation or environment variable names:

```bash
# Dot notation
chbackup --env s3.bucket=other-bucket --env general.log_level=debug create

# Environment variable style (automatically translated)
chbackup --env S3_BUCKET=other-bucket --env CHBACKUP_LOG_LEVEL=debug create
```

Both forms are equivalent. This is useful for one-off overrides without modifying the config file or setting environment variables.
