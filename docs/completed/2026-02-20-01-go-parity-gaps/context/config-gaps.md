# Config Gaps: chbackup (Rust) vs clickhouse-backup (Go)

Comparison date: 2026-02-20
Go source: `Altinity/clickhouse-backup` `pkg/config/config.go` (master branch)
Rust source: `src/config.rs`

---

## 1. Structural Differences

### 1.1 Top-Level Sections

| Go Section | Rust Equivalent | Status |
|------------|----------------|--------|
| `general` | `general` | Present (partial -- see missing params below) |
| `clickhouse` | `clickhouse` | Present (partial) |
| `s3` | `s3` | Present (partial) |
| `gcs` | -- | **Not applicable** (S3-only by design) |
| `cos` | -- | **Not applicable** (S3-only by design) |
| `ftp` | -- | **Not applicable** (S3-only by design) |
| `sftp` | -- | **Not applicable** (S3-only by design) |
| `azblob` | -- | **Not applicable** (S3-only by design) |
| `custom` | -- | **Not applicable** (S3-only by design) |
| `api` | `api` | Present (partial) |
| -- | `backup` | **Rust-only** section (Go puts these in `general` or `s3`) |
| -- | `retention` | **Rust-only** section (Go uses `general.backups_to_keep_*`) |
| -- | `watch` | **Rust-only** section (Go puts watch params in `general`) |

### 1.2 Key Architectural Differences

1. **Backup section**: Rust has a dedicated `backup` section for compression, concurrency overrides, retries, and skip_projections. Go keeps compression in `s3.compression_format`/`s3.compression_level`, concurrency overrides don't exist at backup level.

2. **Retention section**: Rust splits `retention.backups_to_keep_local` / `retention.backups_to_keep_remote` into a dedicated section. Go uses only `general.backups_to_keep_local` / `general.backups_to_keep_remote`.

3. **Watch section**: Rust has a dedicated `watch` section. Go keeps `watch_interval`, `full_interval`, `watch_backup_name_template` in `general`.

4. **S3 path naming**: Go uses `s3.path` for the key prefix. Rust uses `s3.prefix`. This is a **naming incompatibility** that could cause confusion when migrating configs.

5. **Compression location**: Go puts `compression_format` and `compression_level` in the storage section (e.g., `s3.compression_format`). Rust puts them in `backup.compression` and `backup.compression_level`.

---

## 2. Missing Parameters in Rust

### 2.1 GeneralConfig -- Missing Params

| Go Param | Go Type | Go Default | Env Var | Priority | Notes |
|----------|---------|------------|---------|----------|-------|
| `remote_storage` | string | `"none"` | `REMOTE_STORAGE` | **Low** | Rust is S3-only; no multi-backend dispatch needed. Could add for compat if accepting Go configs. |
| `max_file_size` | int64 | `0` | `MAX_FILE_SIZE` | **Low** | Limits individual file size in backup. 0 = unlimited. |
| `allow_empty_backups` | bool | `false` | `ALLOW_EMPTY_BACKUPS` | **Moved** | Rust has this as `backup.allow_empty_backups`. Go has it in `general`. Missing env var overlay `ALLOW_EMPTY_BACKUPS`. |
| `restore_schema_on_cluster` | string | `""` | `RESTORE_SCHEMA_ON_CLUSTER` | **Moved** | Rust has this in `clickhouse.restore_schema_on_cluster`. Go has it in `general`. |
| `upload_by_part` | bool | `true` | `UPLOAD_BY_PART` | **Medium** | Rust always uploads by part (hardcoded behavior). Config flag missing. |
| `download_by_part` | bool | `true` | `DOWNLOAD_BY_PART` | **Medium** | Rust always downloads by part (hardcoded behavior). Config flag missing. |
| `restore_database_mapping` | map[string]string | `{}` | `RESTORE_DATABASE_MAPPING` | **High** | Allows remapping database names during restore. Not implemented in Rust. |
| `restore_table_mapping` | map[string]string | `{}` | `RESTORE_TABLE_MAPPING` | **High** | Allows remapping table names during restore. Not implemented in Rust. |
| `sharded_operation_mode` | string | `""` | `SHARDED_OPERATION_MODE` | **Medium** | Controls sharded backup behavior ("", "none", "table", "database", "first-replica"). |
| `cpu_nice_priority` | int | `15` | `CPU_NICE_PRIORITY` | **Low** | Sets process CPU nice level. |
| `io_nice_priority` | string | `"idle"` | `IO_NICE_PRIORITY` | **Low** | Sets process I/O scheduling class. |
| `delete_batch_size` | int | `1000` | `DELETE_BATCH_SIZE` | **Medium** | Batch size for S3 delete operations. |
| `watch_backup_name_template` | string | `"shard{shard}-{type}-{time:20060102150405}"` | `WATCH_BACKUP_NAME_TEMPLATE` | **Moved** | Rust has this as `watch.name_template`. Different section. |
| `allow_object_disk_streaming` | bool | `false` | `ALLOW_OBJECT_DISK_STREAMING` | **Moved** | Rust has this in `s3.allow_object_disk_streaming`. Go has it in `general`. |

### 2.2 ClickHouseConfig -- Missing Params

| Go Param | Go Type | Go Default | Env Var | Priority | Notes |
|----------|---------|------------|---------|----------|-------|
| `disk_mapping` | map[string]string | `{}` | `CLICKHOUSE_DISK_MAPPING` | **Medium** | Override disk paths during restore. Not implemented. |
| `use_embedded_backup_restore` | bool | `false` | `CLICKHOUSE_USE_EMBEDDED_BACKUP_RESTORE` | **Medium** | Use BACKUP/RESTORE SQL commands instead of FREEZE/ATTACH. |
| `embedded_backup_disk` | string | `""` | `CLICKHOUSE_EMBEDDED_BACKUP_DISK` | **Medium** | Disk name for embedded backup. |

### 2.3 S3Config -- Missing Params

| Go Param | Go Type | Go Default | Env Var | Priority | Notes |
|----------|---------|------------|---------|----------|-------|
| `sse_customer_algorithm` | string | `""` | `S3_SSE_CUSTOMER_ALGORITHM` | **Medium** | SSE-C algorithm (usually AES256). |
| `sse_customer_key` | string | `""` | `S3_SSE_CUSTOMER_KEY` | **Medium** | SSE-C encryption key. |
| `sse_customer_key_md5` | string | `""` | `S3_SSE_CUSTOMER_KEY_MD5` | **Medium** | SSE-C key MD5 hash. |
| `sse_kms_encryption_context` | string | `""` | `S3_SSE_KMS_ENCRYPTION_CONTEXT` | **Low** | KMS encryption context JSON. |
| `use_custom_storage_class` | bool | `false` | `S3_USE_CUSTOM_STORAGE_CLASS` | **Low** | Enable custom storage class mapping. |
| `custom_storage_class_map` | map[string]string | `{}` | `S3_CUSTOM_STORAGE_CLASS_MAP` | **Low** | Maps table patterns to storage classes. |
| `allow_multipart_download` | bool | `false` | `S3_ALLOW_MULTIPART_DOWNLOAD` | **Medium** | Enable parallel range-GET for downloads. |
| `object_labels` | map[string]string | `{}` | `S3_OBJECT_LABELS` | **Low** | S3 object tags applied to uploaded objects. |
| `request_payer` | string | `""` | `S3_REQUEST_PAYER` | **Low** | Requester-pays bucket support. |
| `check_sum_algorithm` | string | `""` | `S3_CHECKSUM_ALGORITHM` | **Low** | S3 additional checksum (CRC32, SHA1, SHA256). |
| `request_content_md5` | bool | `false` | `S3_REQUEST_CONTENT_MD5` | **Low** | Add Content-MD5 header to uploads. |
| `retry_mode` | string | `"standard"` | `S3_RETRY_MODE` | **Medium** | AWS SDK retry mode (standard, adaptive). |
| `delete_concurrency` | int | `10` | `S3_DELETE_CONCURRENCY` | **Medium** | Concurrent delete operations. Rust has no equivalent. |
| `compression_format` | string | `"tar"` | `S3_COMPRESSION_FORMAT` | **Moved** | Rust puts compression in `backup` section. |
| `compression_level` | int | `1` | `S3_COMPRESSION_LEVEL` | **Moved** | Rust puts compression in `backup` section. |

### 2.4 APIConfig -- Missing Params

| Go Param | Go Type | Go Default | Env Var | Priority | Notes |
|----------|---------|------------|---------|----------|-------|
| `enable_pprof` | bool | `false` | `API_ENABLE_PPROF` | **Low** | Go profiling endpoint. Not applicable to Rust. |
| `ca_key_file` | string | `""` | `API_CA_KEY_FILE` | **Low** | Rust has `ca_cert_file` but not `ca_key_file`. Go has both (swapped yaml/envconfig tags -- likely a Go bug). |

---

## 3. Default Value Differences

### 3.1 GeneralConfig Defaults

| Param | Go Default | Rust Default | Match? | Notes |
|-------|-----------|-------------|--------|-------|
| `log_level` | `"info"` | `"info"` | Yes | |
| `backups_to_keep_local` | `0` | `0` | Yes | |
| `backups_to_keep_remote` | `0` | `7` | **NO** | Rust defaults to 7, Go defaults to 0 (unlimited). |
| `upload_concurrency` | `round(sqrt(NumCPU/2))` | `4` (hardcoded) | **NO** | Rust uses hardcoded 4. Go computes dynamically. |
| `download_concurrency` | `NumCPU/2` | `4` (hardcoded) | **NO** | Rust uses hardcoded 4. Go computes dynamically. |
| `upload_max_bytes_per_second` | `0` | `0` | Yes | |
| `download_max_bytes_per_second` | `0` | `0` | Yes | |
| `object_disk_server_side_copy_concurrency` | `32` | `32` | Yes | |
| `use_resumable_state` | `true` | `true` | Yes | |
| `retries_on_failure` | `3` | `3` | Yes | |
| `retries_pause` | `"5s"` | `"5s"` | Yes | |
| `retries_jitter` | `0` (int8) | `30` (u32, percent) | **NO** | Go defaults to 0 jitter. Rust defaults to 30%. |
| `rbac_backup_always` | `true` | -- | N/A | In Rust this is under `clickhouse` and defaults to `false`. See row below. |

### 3.2 ClickHouseConfig Defaults

| Param | Go Default | Rust Default | Match? | Notes |
|-------|-----------|-------------|--------|-------|
| `username` | `"default"` | `"default"` | Yes | |
| `password` | `""` | `""` | Yes | |
| `host` | `"localhost"` | `"localhost"` | Yes | |
| `port` | `9000` | `9000` | Yes | |
| `timeout` | `"30m"` | `"30m"` | Yes | |
| `sync_replicated_tables` | `false` | `true` | **NO** | Rust defaults to `true`, Go defaults to `false`. |
| `log_sql_queries` | `true` | `true` | Yes | |
| `config_dir` | `"/etc/clickhouse-server/"` | `"/etc/clickhouse-server"` | **Minor** | Go has trailing slash. |
| `restart_command` | `"exec:systemctl restart clickhouse-server"` | `"exec:systemctl restart clickhouse-server"` | Yes | |
| `ignore_not_exists_error_during_freeze` | `true` | `true` | Yes | |
| `check_replicas_before_attach` | `true` | `true` | Yes | |
| `backup_mutations` | `true` | `true` | Yes | |
| `restore_as_attach` | `false` | `false` | Yes | |
| `check_parts_columns` | `true` | `true` | Yes | |
| `default_replica_path` | `"/clickhouse/tables/{cluster}/{shard}/{database}/{table}"` | Same | Yes | |
| `default_replica_name` | `"{replica}"` | `"{replica}"` | Yes | |
| `max_connections` | `downloadConcurrency` (=NumCPU/2) | `NumCPU/2` | **Close** | Both dynamically computed. Go uses download_concurrency value. Rust computes independently. Same formula. |
| `rbac_backup_always` | `true` (in general) | `false` (in clickhouse) | **NO** | Different section AND different default. Go=true, Rust=false. |
| `rbac_conflict_resolution` | `"recreate"` (in general) | `"recreate"` (in clickhouse) | Yes (value) | Different section but same default value. |
| `config_backup_always` | `false` (in general) | `false` (in clickhouse) | Yes | Different section but same default. |
| `named_collections_backup_always` | `false` (in general) | `false` (in clickhouse) | Yes | Different section but same default. |
| `skip_tables` | `["system.*", "INFORMATION_SCHEMA.*", "information_schema.*", "_temporary_and_external_tables.*"]` | Same | Yes | |
| `debug` | `false` | `false` | Yes | |

### 3.3 S3Config Defaults

| Param | Go Default | Rust Default | Match? | Notes |
|-------|-----------|-------------|--------|-------|
| `region` | `"us-east-1"` | `"us-east-1"` | Yes | |
| `acl` | `"private"` | `"private"` | Yes | |
| `disable_ssl` | `false` | `false` | Yes | |
| `disable_cert_verification` | `false` | `false` | Yes | |
| `storage_class` | `"STANDARD"` | `"STANDARD"` | Yes | |
| `concurrency` | `downloadConcurrency + 1` (=NumCPU/2+1) | `1` | **NO** | Rust defaults to 1. Go defaults to NumCPU/2+1. |
| `max_parts_count` | `4000` | `10000` | **NO** | Rust defaults to 10000. Go defaults to 4000. |
| `chunk_size` | `5242880` (5 MiB) | `0` (auto) | **NO** | Go defaults to 5 MiB. Rust defaults to 0 (auto-calculated). |

### 3.4 APIConfig Defaults

| Param | Go Default | Rust Default | Match? | Notes |
|-------|-----------|-------------|--------|-------|
| `listen` | `"localhost:7171"` | `"localhost:7171"` | Yes | |
| `enable_metrics` | `true` | `true` | Yes | |
| `create_integration_tables` | `false` | `true` | **NO** | Rust defaults to `true`, Go defaults to `false`. |
| `complete_resumable_after_restart` | `true` | `true` | Yes | |
| `allow_parallel` | `false` | `false` | Yes | |
| `watch_is_main_process` | `false` | `false` | Yes | |

### 3.5 Backup Section (Rust-only, cross-reference)

| Rust Param | Rust Default | Go Equivalent | Go Default | Match? | Notes |
|-----------|-------------|---------------|------------|--------|-------|
| `compression` | `"lz4"` | `s3.compression_format` | `"tar"` | **NO** | Go defaults to `"tar"` (plain tarball). Rust defaults to `"lz4"`. Go explicitly FORBIDS `"lz4"` in validation ("clickhouse already compressed data by lz4"). |
| `compression_level` | `1` | `s3.compression_level` | `1` | Yes | |
| `retries_on_failure` | `5` | -- | -- | N/A | Rust-only override. |
| `retries_duration` | `"10s"` | -- | -- | N/A | Rust-only override. |
| `retries_jitter` | `0.1` | -- | -- | N/A | Rust-only override. |
| `object_disk_copy_concurrency` | `8` | -- | -- | N/A | Rust-only. |

---

## 4. Missing Env Var Overlays in Rust

Rust `apply_env_overlay()` handles only ~18 env vars. Go handles ALL params via `envconfig` tags. The following Go env vars have **no Rust equivalent**:

### 4.1 General Section Env Vars Missing

| Env Var | Go Param | Currently in Rust? |
|---------|----------|-------------------|
| `REMOTE_STORAGE` | `general.remote_storage` | No (N/A for S3-only) |
| `MAX_FILE_SIZE` | `general.max_file_size` | No |
| `BACKUPS_TO_KEEP_LOCAL` | `general.backups_to_keep_local` | No |
| `BACKUPS_TO_KEEP_REMOTE` | `general.backups_to_keep_remote` | No |
| `LOG_LEVEL` | `general.log_level` | **Partial** -- Rust uses `CHBACKUP_LOG_LEVEL` not `LOG_LEVEL` |
| `LOG_FORMAT` | -- | **Partial** -- Rust uses `CHBACKUP_LOG_FORMAT` not standard name |
| `ALLOW_EMPTY_BACKUPS` | `general.allow_empty_backups` | No |
| `DOWNLOAD_CONCURRENCY` | `general.download_concurrency` | No |
| `UPLOAD_CONCURRENCY` | `general.upload_concurrency` | No |
| `UPLOAD_MAX_BYTES_PER_SECOND` | `general.upload_max_bytes_per_second` | No |
| `DOWNLOAD_MAX_BYTES_PER_SECOND` | `general.download_max_bytes_per_second` | No |
| `OBJECT_DISK_SERVER_SIDE_COPY_CONCURRENCY` | `general.object_disk_server_side_copy_concurrency` | No |
| `ALLOW_OBJECT_DISK_STREAMING` | `general.allow_object_disk_streaming` | No |
| `USE_RESUMABLE_STATE` | `general.use_resumable_state` | No |
| `RESTORE_SCHEMA_ON_CLUSTER` | `general.restore_schema_on_cluster` | No |
| `UPLOAD_BY_PART` | `general.upload_by_part` | No |
| `DOWNLOAD_BY_PART` | `general.download_by_part` | No |
| `RESTORE_DATABASE_MAPPING` | `general.restore_database_mapping` | No |
| `RESTORE_TABLE_MAPPING` | `general.restore_table_mapping` | No |
| `RETRIES_ON_FAILURE` | `general.retries_on_failure` | No |
| `RETRIES_PAUSE` | `general.retries_pause` | No |
| `RETRIES_JITTER` | `general.retries_jitter` | No |
| `WATCH_BACKUP_NAME_TEMPLATE` | `general.watch_backup_name_template` | No |
| `SHARDED_OPERATION_MODE` | `general.sharded_operation_mode` | No |
| `CPU_NICE_PRIORITY` | `general.cpu_nice_priority` | No |
| `IO_NICE_PRIORITY` | `general.io_nice_priority` | No |
| `RBAC_BACKUP_ALWAYS` | `general.rbac_backup_always` | No |
| `RBAC_CONFLICT_RESOLUTION` | `general.rbac_conflict_resolution` | No |
| `CONFIG_BACKUP_ALWAYS` | `general.config_backup_always` | No |
| `NAMED_COLLECTIONS_BACKUP_ALWAYS` | `general.named_collections_backup_always` | No |
| `DELETE_BATCH_SIZE` | `general.delete_batch_size` | No |

### 4.2 ClickHouse Section Env Vars Missing

| Env Var | Go Param | Currently in Rust? |
|---------|----------|-------------------|
| `CLICKHOUSE_DISK_MAPPING` | `clickhouse.disk_mapping` | No |
| `CLICKHOUSE_SKIP_TABLES` | `clickhouse.skip_tables` | No |
| `CLICKHOUSE_SKIP_TABLE_ENGINES` | `clickhouse.skip_table_engines` | No |
| `CLICKHOUSE_SKIP_DISKS` | `clickhouse.skip_disks` | No |
| `CLICKHOUSE_SKIP_DISK_TYPES` | `clickhouse.skip_disk_types` | No |
| `CLICKHOUSE_TIMEOUT` | `clickhouse.timeout` | No |
| `CLICKHOUSE_FREEZE_BY_PART` | `clickhouse.freeze_by_part` | No |
| `CLICKHOUSE_FREEZE_BY_PART_WHERE` | `clickhouse.freeze_by_part_where` | No |
| `CLICKHOUSE_USE_EMBEDDED_BACKUP_RESTORE` | `clickhouse.use_embedded_backup_restore` | No |
| `CLICKHOUSE_EMBEDDED_BACKUP_DISK` | `clickhouse.embedded_backup_disk` | No |
| `CLICKHOUSE_BACKUP_MUTATIONS` | `clickhouse.backup_mutations` | No |
| `CLICKHOUSE_RESTORE_AS_ATTACH` | `clickhouse.restore_as_attach` | No |
| `CLICKHOUSE_RESTORE_DISTRIBUTED_CLUSTER` | `clickhouse.restore_distributed_cluster` | No |
| `CLICKHOUSE_CHECK_PARTS_COLUMNS` | `clickhouse.check_parts_columns` | No |
| `CLICKHOUSE_SECURE` | `clickhouse.secure` | No |
| `CLICKHOUSE_SKIP_VERIFY` | `clickhouse.skip_verify` | No |
| `CLICKHOUSE_SYNC_REPLICATED_TABLES` | `clickhouse.sync_replicated_tables` | No |
| `CLICKHOUSE_LOG_SQL_QUERIES` | `clickhouse.log_sql_queries` | No |
| `CLICKHOUSE_CONFIG_DIR` | `clickhouse.config_dir` | No |
| `CLICKHOUSE_RESTART_COMMAND` | `clickhouse.restart_command` | No |
| `CLICKHOUSE_IGNORE_NOT_EXISTS_ERROR_DURING_FREEZE` | `clickhouse.ignore_not_exists_error_during_freeze` | No |
| `CLICKHOUSE_CHECK_REPLICAS_BEFORE_ATTACH` | `clickhouse.check_replicas_before_attach` | No |
| `CLICKHOUSE_DEFAULT_REPLICA_PATH` | `clickhouse.default_replica_path` | No |
| `CLICKHOUSE_DEFAULT_REPLICA_NAME` | `clickhouse.default_replica_name` | No |
| `CLICKHOUSE_TLS_KEY` | `clickhouse.tls_key` | No |
| `CLICKHOUSE_TLS_CERT` | `clickhouse.tls_cert` | No |
| `CLICKHOUSE_TLS_CA` | `clickhouse.tls_ca` | No |
| `CLICKHOUSE_MAX_CONNECTIONS` | `clickhouse.max_connections` | No |
| `CLICKHOUSE_DEBUG` | `clickhouse.debug` | No |

### 4.3 S3 Section Env Vars Missing

| Env Var | Go Param | Currently in Rust? |
|---------|----------|-------------------|
| `S3_ACL` | `s3.acl` | No |
| `S3_DISABLE_SSL` | `s3.disable_ssl` | No |
| `S3_DISABLE_CERT_VERIFICATION` | `s3.disable_cert_verification` | No |
| `S3_STORAGE_CLASS` | `s3.storage_class` | No |
| `S3_SSE` | `s3.sse` | No |
| `S3_SSE_KMS_KEY_ID` | `s3.sse_kms_key_id` | No |
| `S3_SSE_CUSTOMER_ALGORITHM` | `s3.sse_customer_algorithm` | No |
| `S3_SSE_CUSTOMER_KEY` | `s3.sse_customer_key` | No |
| `S3_SSE_CUSTOMER_KEY_MD5` | `s3.sse_customer_key_md5` | No |
| `S3_SSE_KMS_ENCRYPTION_CONTEXT` | `s3.sse_kms_encryption_context` | No |
| `S3_CONCURRENCY` | `s3.concurrency` | No |
| `S3_MAX_PARTS_COUNT` | `s3.max_parts_count` | No |
| `S3_ALLOW_MULTIPART_DOWNLOAD` | `s3.allow_multipart_download` | No |
| `S3_OBJECT_LABELS` | `s3.object_labels` | No |
| `S3_REQUEST_PAYER` | `s3.request_payer` | No |
| `S3_CHECKSUM_ALGORITHM` | `s3.check_sum_algorithm` | No |
| `S3_REQUEST_CONTENT_MD5` | `s3.request_content_md5` | No |
| `S3_RETRY_MODE` | `s3.retry_mode` | No |
| `S3_CHUNK_SIZE` | `s3.chunk_size` | No |
| `S3_DELETE_CONCURRENCY` | `s3.delete_concurrency` | No |
| `S3_OBJECT_DISK_PATH` | `s3.object_disk_path` | No |
| `S3_DEBUG` | `s3.debug` | No |

### 4.4 API Section Env Vars Missing

| Env Var | Go Param | Currently in Rust? |
|---------|----------|-------------------|
| `API_ENABLE_METRICS` | `api.enable_metrics` | No |
| `API_ENABLE_PPROF` | `api.enable_pprof` | No (N/A) |
| `API_USERNAME` | `api.username` | No |
| `API_PASSWORD` | `api.password` | No |
| `API_SECURE` | `api.secure` | No |
| `API_CERTIFICATE_FILE` | `api.certificate_file` | No |
| `API_PRIVATE_KEY_FILE` | `api.private_key_file` | No |
| `API_CA_KEY_FILE` | `api.ca_key_file` | No |
| `API_CA_CERT_FILE` | `api.ca_cert_file` | No |
| `API_CREATE_INTEGRATION_TABLES` | `api.create_integration_tables` | No |
| `API_INTEGRATION_TABLES_HOST` | `api.integration_tables_host` | No |
| `API_ALLOW_PARALLEL` | `api.allow_parallel` | No |
| `API_COMPLETE_RESUMABLE_AFTER_RESTART` | `api.complete_resumable_after_restart` | No |
| `WATCH_IS_MAIN_PROCESS` | `api.watch_is_main_process` | No |

### 4.5 Summary of Env Var Coverage

| Section | Go Env Vars | Rust Env Vars | Coverage |
|---------|------------|---------------|----------|
| General | ~31 | 2 (`CHBACKUP_LOG_LEVEL`, `CHBACKUP_LOG_FORMAT`) | ~6% |
| ClickHouse | ~29 | 5 (`HOST`, `PORT`, `USERNAME`, `PASSWORD`, `DATA_PATH`) | ~17% |
| S3 | ~28 | 8 (`BUCKET`, `REGION`, `ENDPOINT`, `PREFIX`, `ACCESS_KEY`, `SECRET_KEY`, `ASSUME_ROLE_ARN`, `FORCE_PATH_STYLE`) | ~29% |
| API | ~15 | 1 (`API_LISTEN`) | ~7% |
| Watch | 2 | 2 (`WATCH_INTERVAL`, `FULL_INTERVAL`) | 100% |
| **Total** | **~105** | **18** | **~17%** |

---

## 5. Missing Validation Rules

### 5.1 Go Validations Not Present in Rust

| Validation | Go Behavior | Rust Status |
|-----------|------------|-------------|
| S3 retry mode | Validates against AWS SDK `ParseRetryMode()` | **Missing** (no `retry_mode` field) |
| Compression format != "lz4" | Go **rejects** `"lz4"` as compression format | **CONFLICT** -- Rust defaults to `"lz4"` and allows it |
| Compression format known | Validates against `ArchiveExtensions` map | Rust validates against `"lz4"/"zstd"/"gzip"/"none"` -- different set |
| S3 storage class | Validates against `s3types.StorageClass.Values()` | **Missing** |
| S3 multipart download + concurrency | `allow_multipart_download` requires `concurrency > 1` | **Missing** (no `allow_multipart_download` field) |
| API TLS cert validation | Loads X509 key pair at config validation time | **Missing** |
| ClickHouse timeout for embedded backup | Requires >= 240m when `use_embedded_backup_restore=true` | **Missing** (no `use_embedded_backup_restore` field) |
| `freeze_by_part` vs `use_embedded_backup_restore` | Rejects combination | **Missing** (no `use_embedded_backup_restore` field) |
| COS/FTP/Azure timeout parsing | Validates duration format | N/A (S3-only) |
| FTP concurrency >= upload/download concurrency | Cross-field validation | N/A (S3-only) |
| Custom command timeout parsing | Validates non-empty duration | N/A (no custom backend) |
| Retries pause duration parsing | Validates parseable duration | **Missing** -- Rust does not validate `retries_pause` format |
| Watch interval duration parsing | Validates parseable duration | **Partial** -- Only validated when `watch.enabled=true` |
| Full interval duration parsing | Validates parseable duration | **Partial** -- Only validated when `watch.enabled=true` |

---

## 6. Type Differences

| Param | Go Type | Rust Type | Issue |
|-------|---------|-----------|-------|
| `general.upload_concurrency` | `uint8` (0-255) | `u32` | Wider type in Rust. Not a problem functionally. |
| `general.download_concurrency` | `uint8` (0-255) | `u32` | Same as above. |
| `general.retries_on_failure` | `int` | `u32` | Go allows negative (meaningless). Rust is unsigned. |
| `general.retries_jitter` | `int8` (-128 to 127, represents %) | `u32` (represents %) | Different signedness. Rust-only `backup.retries_jitter` uses `f64` (0.0-1.0 fraction). |
| `clickhouse.port` | `uint` | `u16` | Go uint is platform-dependent (32 or 64 bit). Rust u16 limits to 65535 which is correct for ports. |
| `clickhouse.max_connections` | `int` | `u32` | Go allows negative. Rust unsigned. |
| `s3.max_parts_count` | `int64` | `u32` | Narrower in Rust. Could overflow for very large values but 10000 is well within u32 range. |
| `s3.concurrency` | `int` | `u32` | Rust unsigned. Both fine. |
| `s3.chunk_size` | `int64` | `u64` | Rust unsigned. Go allows negative (meaningless). |

---

## 7. Naming Differences (YAML Keys)

Fields present in both but with different YAML key names:

| Go YAML Key | Rust YAML Key | Go Section | Rust Section | Impact |
|-------------|---------------|-----------|--------------|--------|
| `s3.path` | `s3.prefix` | `s3` | `s3` | **High** -- Config files not interchangeable |
| `s3.compression_format` | `backup.compression` | `s3` | `backup` | **High** -- Different section + key name |
| `s3.compression_level` | `backup.compression_level` | `s3` | `backup` | **Medium** -- Different section |
| `general.allow_empty_backups` | `backup.allow_empty_backups` | `general` | `backup` | **Medium** -- Different section |
| `general.restore_schema_on_cluster` | `clickhouse.restore_schema_on_cluster` | `general` | `clickhouse` | **Medium** -- Different section |
| `general.allow_object_disk_streaming` | `s3.allow_object_disk_streaming` | `general` | `s3` | **Low** -- Different section |
| `general.watch_interval` | `watch.watch_interval` | `general` | `watch` | **Low** -- Different section |
| `general.full_interval` | `watch.full_interval` | `general` | `watch` | **Low** -- Different section |
| `general.watch_backup_name_template` | `watch.name_template` | `general` | `watch` | **Low** -- Different section + key |
| `general.rbac_backup_always` | `clickhouse.rbac_backup_always` | `general` | `clickhouse` | **Low** -- Different section |
| `general.rbac_conflict_resolution` | `clickhouse.rbac_resolve_conflicts` | `general` | `clickhouse` | **Low** -- Different section + key |
| `general.config_backup_always` | `clickhouse.config_backup_always` | `general` | `clickhouse` | **Low** -- Different section |
| `general.named_collections_backup_always` | `clickhouse.named_collections_backup_always` | `general` | `clickhouse` | **Low** -- Different section |

---

## 8. Critical Issues Summary

### 8.1 Severity: HIGH

| # | Issue | Details |
|---|-------|---------|
| 1 | **Compression format conflict** | Go explicitly **rejects** `"lz4"` (validation error: "clickhouse already compressed data by lz4"). Rust **defaults** to `"lz4"`. Go's default is `"tar"` (plain tar, no additional compression). This means Rust is applying LZ4 on top of already-LZ4-compressed ClickHouse data by default, which is wasteful. |
| 2 | **`restore_database_mapping` missing** | Users rely on this to restore databases under different names. No workaround in Rust. |
| 3 | **`restore_table_mapping` missing** | Users rely on this to restore tables under different names. No workaround in Rust. |
| 4 | **`s3.path` vs `s3.prefix` naming** | Go config files use `path:`, Rust expects `prefix:`. Users migrating from Go will hit silent misconfiguration -- the prefix will default to `"chbackup"` instead of their intended value. |
| 5 | **Env var coverage at ~17%** | Kubernetes deployments heavily depend on env var configuration. Only 18 of ~105 Go env vars are supported. |

### 8.2 Severity: MEDIUM

| # | Issue | Details |
|---|-------|---------|
| 6 | **`general.backups_to_keep_remote` default 7 vs 0** | Rust will auto-delete remote backups beyond 7. Go keeps all by default. Users without explicit config will lose backups. |
| 7 | **`sync_replicated_tables` default true vs false** | Rust runs SYSTEM SYNC REPLICA before every FREEZE by default. Go does not. This adds latency and may cause issues on large clusters. |
| 8 | **`s3.concurrency` default 1 vs NumCPU/2+1** | Rust's S3 per-file multipart concurrency is 1 (serial). Go uses NumCPU/2+1. Large file uploads will be significantly slower in Rust. |
| 9 | **`s3.max_parts_count` default 10000 vs 4000** | Different chunk sizing for multipart uploads. Not a correctness issue but affects behavior. |
| 10 | **`api.create_integration_tables` default true vs false** | Rust auto-creates ClickHouse integration tables. Go does not. Could cause unexpected DDL on startup. |
| 11 | **`rbac_backup_always` default false vs true** | Go includes RBAC objects by default. Rust does not. Backups may silently omit access control definitions. |
| 12 | **Upload/download concurrency not CPU-dynamic** | Go computes `sqrt(NumCPU/2)` and `NumCPU/2`. Rust hardcodes `4`. On high-core machines, Rust underutilizes. On single-core, Rust over-allocates. |
| 13 | **`disk_mapping` missing** | Cannot override disk paths during restore (needed for migration between hosts with different disk layouts). |
| 14 | **`delete_batch_size` missing** | Rust may use different batch sizes for S3 DeleteObjects calls. |
| 15 | **SSE-C fields missing** | `sse_customer_algorithm`, `sse_customer_key`, `sse_customer_key_md5` -- needed for customer-managed encryption keys. |
| 16 | **`s3.chunk_size` default 0 vs 5 MiB** | Different multipart chunk sizing behavior. |

### 8.3 Severity: LOW

| # | Issue | Details |
|---|-------|---------|
| 17 | `config_dir` trailing slash | Go: `"/etc/clickhouse-server/"`, Rust: `"/etc/clickhouse-server"`. May affect path joining. |
| 18 | `sharded_operation_mode` missing | Advanced multi-shard coordination. |
| 19 | `cpu_nice_priority` / `io_nice_priority` missing | Process scheduling. |
| 20 | `upload_by_part` / `download_by_part` flags missing | Always true in Rust (correct behavior), but no config toggle. |
| 21 | `use_embedded_backup_restore` missing | BACKUP/RESTORE SQL mode (ClickHouse 23.3+). |
| 22 | `max_file_size` missing | File size limit per backup entry. |
| 23 | S3 object labels, request payer, checksum algorithm missing | Niche S3 features. |
| 24 | `enable_pprof` missing | Not applicable to Rust (Go profiling). |
| 25 | `s3.retry_mode` missing | AWS SDK retry configuration. |
| 26 | `s3.delete_concurrency` missing | Concurrent S3 delete operations. |
| 27 | `s3.allow_multipart_download` missing | Parallel range-GET for downloads. |
| 28 | `use_custom_storage_class` / `custom_storage_class_map` missing | Table-level storage class routing. |

---

## 9. Rust-Only Features (Not in Go)

| Param | Section | Notes |
|-------|---------|-------|
| `backup.upload_concurrency` | backup | Backup-level override for upload concurrency |
| `backup.download_concurrency` | backup | Backup-level override for download concurrency |
| `backup.object_disk_copy_concurrency` | backup | Separate concurrency for object disk copies during upload |
| `backup.upload_max_bytes_per_second` | backup | Backup-level rate limit override |
| `backup.download_max_bytes_per_second` | backup | Backup-level rate limit override |
| `backup.retries_on_failure` | backup | Backup-level retry override |
| `backup.retries_duration` | backup | Backup-level retry pause override |
| `backup.retries_jitter` | backup | Backup-level jitter override (float) |
| `backup.skip_projections` | backup | Projection exclusion patterns |
| `retention.backups_to_keep_local` | retention | Dedicated retention section |
| `retention.backups_to_keep_remote` | retention | Dedicated retention section |
| `watch.enabled` | watch | Explicit enable flag (Go uses presence of `--watch` flag) |
| `watch.tables` | watch | Table filter for watch-mode backups |
| `watch.max_consecutive_errors` | watch | Error threshold before watch exits |
| `watch.retry_interval` | watch | Interval between retries after error |
| `watch.delete_local_after_upload` | watch | Auto-clean local after upload |
| `clickhouse.mutation_wait_timeout` | clickhouse | Timeout for mutation sync wait |
| `clickhouse.data_path` | clickhouse | ClickHouse data directory path |

---

## 10. Recommendations

### Immediate (before any release)

1. **Fix compression default**: Change `backup.compression` default from `"lz4"` to `"tar"` (plain tarball) to match Go, or at minimum `"zstd"` which is actually useful double-compression. Go forbids `"lz4"` entirely.

2. **Fix `general.backups_to_keep_remote` default**: Change from `7` to `0` to match Go and prevent accidental backup deletion.

3. **Fix `sync_replicated_tables` default**: Change from `true` to `false` to match Go.

4. **Fix `api.create_integration_tables` default**: Change from `true` to `false` to match Go.

5. **Fix `rbac_backup_always` default**: Change from `false` to `true` to match Go.

### Short-term (env var parity)

6. **Expand env var overlay**: Add ALL Go-compatible env var names (using the same names, e.g., `CLICKHOUSE_HOST` not `CHBACKUP_CLICKHOUSE_HOST`). This is critical for Kubernetes deployments. Consider keeping `CHBACKUP_*` as aliases.

### Medium-term (feature parity)

7. **Add `restore_database_mapping`**: Map[String, String] for database name remapping during restore.

8. **Add `restore_table_mapping`**: Map[String, String] for table name remapping during restore.

9. **Accept `s3.path` as alias for `s3.prefix`**: Use `#[serde(alias = "path")]` to accept Go config files.

10. **Make concurrency defaults CPU-dynamic**: Match Go's `sqrt(NumCPU/2)` for upload and `NumCPU/2` for download.

11. **Add `s3.concurrency` CPU-dynamic default**: Match Go's `NumCPU/2+1`.

12. **Add `disk_mapping`**: Required for host migration scenarios.

13. **Add SSE-C fields**: `sse_customer_algorithm`, `sse_customer_key`, `sse_customer_key_md5`.

### Long-term (nice-to-have)

14. Add `delete_batch_size`, `sharded_operation_mode`, `cpu_nice_priority`, `io_nice_priority`.

15. Add `upload_by_part` / `download_by_part` toggles (even if implementation always streams by part).

16. Add `use_embedded_backup_restore` + `embedded_backup_disk` for ClickHouse 23.3+ native backup.

17. Add S3 `allow_multipart_download`, `object_labels`, `request_payer`, `retry_mode`.
