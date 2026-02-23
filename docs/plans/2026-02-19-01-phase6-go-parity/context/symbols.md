# Symbol Analysis

## Key Symbols for Modification

### src/config.rs
- `default_ch_timeout()` → change "5m" to "30m"
- `default_max_connections()` → change 1 to dynamic NumCPU/2
- `default_replica_path()` → add {cluster}
- `default_skip_tables()` → add _temporary_and_external_tables.*
- `check_parts_columns: false` → change to `true` in Default impl
- ACL default: add `default_s3_acl() -> String` returning `"private"`
- `ClickHouseConfig.debug: bool` (line 194) — never referenced outside config
- `S3Config.debug: bool` (line 318) — never referenced outside config

### src/storage/s3.rs
- `S3Client::new()` (line 44) — add assume_role_arn logic, cert verification
- `S3Client.storage_class: String` (line 32) — uppercase before use
- `put_object_with_options()` (line 167) — add ACL parameter
- `create_multipart_upload()` (line 466) — add ACL parameter
- `copy_object()` (line 659) — add ACL + size check for multipart
- `copy_object_with_retry()` (line 782) — add jitter to backoff delays
- Need new: `copy_object_multipart()` using UploadPartCopy

### src/backup/mod.rs
- `create()` function — add freeze_by_part check, failure cleanup
- Error handling (lines 373-389) — add code 218
- `parse_partition_list()` (line 44) — add "all" handling

### src/backup/freeze.rs
- `freeze_table()` — add per-partition FREEZE path

### src/restore/mod.rs
- `restore()` (line 79) — add partition filtering, skip_empty_tables
- Need partition matching against manifest parts

### src/restore/schema.rs
- Add check_replicas_before_attach query

### src/list.rs
- `retention_remote()` — add incremental chain check
- Add output format support (json/yaml/csv/tsv)
- Add latest/previous resolution

### src/server/routes.rs
- All `StatusCode::CONFLICT` (12 occurrences) → `StatusCode::LOCKED`
- Health handler — change from text to JSON
- Actions handler — wire actual dispatch
- List response — add fields

### src/server/mod.rs
- Watch loop exit handler — add watch_is_main_process check

### src/cli.rs
- Add `--format` flag to list commands
