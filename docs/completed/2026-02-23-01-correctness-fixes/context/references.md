# Symbol and Reference Analysis

## Issue 1: Path Traversal -- url_encode Functions

### Symbol Locations

| Symbol | File | Line | Visibility | Preserves `/` |
|--------|------|------|------------|---------------|
| `url_encode_path` | src/backup/collect.rs | 29 | `pub` | YES |
| `url_encode` | src/download/mod.rs | 41 | `fn` (private) | YES |
| `url_encode_component` | src/upload/mod.rs | 55 | `fn` (private) | NO |
| `url_encode` | src/restore/attach.rs | 844 | `pub(crate)` | YES |

### Call Sites for `backup::collect::url_encode_path` (3 references)

| File | Line | Context |
|------|------|---------|
| src/backup/collect.rs | 29 | Definition |
| src/backup/collect.rs | 383 | `collect_parts()` -- encoding db name for shadow staging dir |
| src/backup/collect.rs | 384 | `collect_parts()` -- encoding table name for shadow staging dir |

### Call Sites for `download::url_encode` (6 callers via LSP)

| File | Line | Context |
|------|------|---------|
| src/download/mod.rs | 176 | `find_existing_part()` -- encoding db for hardlink dedup search |
| src/download/mod.rs | 177 | `find_existing_part()` -- encoding table for hardlink dedup search |
| src/download/mod.rs | 522 | `download()` parallel task -- encoding db for shadow path |
| src/download/mod.rs | 523 | `download()` parallel task -- encoding table for shadow path |
| src/download/mod.rs | 847 | `download()` metadata save -- encoding db for metadata dir |
| src/download/mod.rs | 852 | `download()` metadata save -- encoding table for metadata file |

### Call Sites for `upload::url_encode_component` (5 callers via LSP)

| File | Line | Context |
|------|------|---------|
| src/upload/mod.rs | 81 | `s3_key_for_part()` -- encoding db for S3 key |
| src/upload/mod.rs | 82 | `s3_key_for_part()` -- encoding table for S3 key |
| src/upload/mod.rs | 345 | `upload()` S3 disk metadata key -- encoding db |
| src/upload/mod.rs | 346 | `upload()` S3 disk metadata key -- encoding table |
| src/upload/mod.rs | 814 | `upload()` S3 disk metadata key -- encoding db |
| src/upload/mod.rs | 815 | `upload()` S3 disk metadata key -- encoding table |
| src/upload/mod.rs | 1084 | `find_part_dir()` -- encoding db/table for local path lookup |
| src/upload/mod.rs | 1085 | `find_part_dir()` -- encoding table for local path lookup |

### Call Sites for `restore::attach::url_encode` (4 callers via LSP)

| File | Line | Context |
|------|------|---------|
| src/restore/attach.rs | 316 | `restore_s3_disk_parts()` -- encoding source db |
| src/restore/attach.rs | 317 | `restore_s3_disk_parts()` -- encoding source table |
| src/restore/attach.rs | 556 | `attach_parts_inner()` -- encoding source db |
| src/restore/attach.rs | 557 | `attach_parts_inner()` -- encoding source table |
| src/restore/mod.rs | 994 | `try_attach_table_mode()` -- encoding source db |
| src/restore/mod.rs | 995 | `try_attach_table_mode()` -- encoding source table |

### Path Traversal Risk Assessment

All path construction uses `url_encode(db)` and `url_encode(table)` where db/table come from:
1. **BackupManifest** -- deserialized from `metadata.json` downloaded from S3
2. **ClickHouse system tables** -- queried from local ClickHouse instance

Neither source is user-controlled in the intended deployment model (chbackup runs on the ClickHouse host). However, a compromised S3 bucket or a crafted manifest could inject `..` into db/table names. The current `url_encode` functions DO encode `..` characters (`.` is preserved but path separators are not), so `../` would become `..%2F` in most variants. But `/` is preserved by 3 of 4 implementations, so `foo/../../etc/passwd` would NOT be sanitized.

---

## Issue 2: disable_cert_verification -- S3Client Construction

### Symbol: `S3Client::new(config: &S3Config)`

- **File**: src/storage/s3.rs, line 60
- **Type**: `pub async fn(config: &S3Config) -> Result<Self>`
- **Callers** (13 via grep):
  - src/main.rs: lines 207, 237, 313, 375, 412, 428, 519, 542, 572, 664
  - src/server/routes.rs: line 1320
  - src/restore/mod.rs: line 412
  - src/watch/mod.rs: line 651

### Relevant Builder Type

- `s3_config_builder` at line 141: type `aws_sdk_s3::config::Builder` (confirmed via LSP hover, size=496 bytes)
- The builder is constructed via `aws_sdk_s3::config::Builder::from(&effective_sdk_config)` at line 141
- Current broken code at lines 150-162: `std::env::set_var("AWS_CA_BUNDLE", "")` -- process-global, does not work

### AWS SDK Dependency Chain (verified via cargo tree)

```
aws-sdk-s3 v1.123.0
  -> aws-smithy-runtime v1.10.1
    -> hyper-rustls v0.24.2 (rustls v0.21.12) -- legacy
    -> hyper-rustls v0.27.7 (rustls v0.23.36) -- modern
```

### Fields on S3Config Relevant to TLS

| Field | Type | Default | Used in S3Client::new? |
|-------|------|---------|----------------------|
| `disable_ssl` | bool | false | NO (dead) |
| `disable_cert_verification` | bool | false | YES (broken: sets env var) |

---

## Issue 3: S3 Unit Tests -- mock_s3_client

### Symbol: `mock_s3_client(bucket, prefix) -> S3Client`

- **File**: src/storage/s3.rs, line 1520
- **Visibility**: `fn` (private, test-only)
- **References** (8 callers in tests):
  - line 1338: `test_full_key_with_prefix`
  - line 1347: `test_full_key_with_trailing_slash_prefix`
  - line 1356: `test_full_key_empty_prefix`
  - line 1365: `test_full_key_nested_prefix`
  - line 1430: `test_copy_object_builds_correct_source`
  - line 1448: `test_copy_object_with_retry_no_streaming_when_disabled`
  - line 1470: `test_put_object_retry_config`
  - line 1496: `test_upload_part_retry_config`

### Test Classification

| Test | Type | Needs Real Client? | Uses mock_s3_client? |
|------|------|-------------------|---------------------|
| test_s3_config_defaults | sync | NO | NO |
| test_full_key_with_prefix | sync | NO (only `full_key()`) | YES |
| test_full_key_with_trailing_slash_prefix | sync | NO (only `full_key()`) | YES |
| test_full_key_empty_prefix | sync | NO (only `full_key()`) | YES |
| test_full_key_nested_prefix | sync | NO (only `full_key()`) | YES |
| test_multipart_chunk_calculation | sync | NO (only `calculate_chunk_size()`) | NO |
| test_calculate_chunk_size_auto | sync | NO | NO |
| test_calculate_chunk_size_explicit | sync | NO | NO |
| test_calculate_chunk_size_minimum | sync | NO | NO |
| test_copy_object_builds_correct_source | sync | NO (only `full_key()`) | YES |
| test_copy_object_with_retry_no_streaming_when_disabled | async | YES (makes S3 call) | YES |
| test_put_object_retry_config | async | YES (makes S3 call) | YES |
| test_upload_part_retry_config | async | YES (makes S3 call) | YES |

The 5 sync tests that use `mock_s3_client` only need `bucket` and `prefix` fields (for `full_key()`).

---

## Issue 5: check_parts_columns -- Control Flow

### Symbol: `filter_benign_type_drift(inconsistencies: Vec<ColumnInconsistency>) -> Vec<ColumnInconsistency>`

- **File**: src/backup/mod.rs, line 803 (definition confirmed via LSP documentSymbol)
- **Callers** (2 via LSP incomingCalls):
  - src/backup/mod.rs, line 200: `create()` -- the main backup flow
  - src/backup/mod.rs, line 1048: `test_parts_columns_check_skip_benign_types` -- unit test

### Current Control Flow (backup/mod.rs:192-226)

```
if config.clickhouse.check_parts_columns && !skip_check_parts_columns:
  query check_parts_columns()
  -> Ok(inconsistencies):
    actionable = filter_benign_type_drift(inconsistencies)
    if !actionable.is_empty():
      for each: warn!(...)
      info!("proceeding anyway")  // <-- THIS IS THE BUG: never fails
    else:
      info!("check passed")
  -> Err(e):
    warn!("check failed, continuing anyway")
```

### CLI Flag: `skip_check_parts_columns`

- Defined in src/cli.rs:87 (Create command) and src/cli.rs:212 (CreateRemote command)
- Passed through main.rs:159,188 -> backup::create() parameter
- Also referenced in server/routes.rs:341,419,723,750,984,1030
- Default: `false` (the check runs when `check_parts_columns` config is true)

---

## Issue 6: --env Format Support

### Symbol: `apply_cli_env_overrides(&mut self, overrides: &[String]) -> Result<()>`

- **File**: src/config.rs, line 1100
- **Called from**: src/config.rs, line 857 (`Config::load()`)
- Delegates to `set_field(key, value)` which ONLY accepts dot-notation

### Symbol: `apply_env_overlay(&mut self)`

- **File**: src/config.rs, line 871
- **Called from**: src/config.rs, line 856 (`Config::load()`)
- Uses uppercase env var names: `S3_BUCKET`, `CLICKHOUSE_HOST`, etc.

### Mapping Table (extracted from apply_env_overlay)

54+ env var -> config field mappings exist in `apply_env_overlay()`. These need to be exposed as a reverse lookup for `apply_cli_env_overrides()`.

Key examples:
- `S3_BUCKET` -> `s3.bucket`
- `CLICKHOUSE_HOST` -> `clickhouse.host`
- `CLICKHOUSE_PORT` -> `clickhouse.port`
- `API_LISTEN` -> `api.listen`
- `WATCH_INTERVAL` -> `watch.watch_interval`

---

## Issue 7: DRY url_encode -- New Module

### New File: `src/path_encoding.rs`

- Must be declared in `src/lib.rs` as `pub mod path_encoding;`
- Current `lib.rs` has 20 module declarations (lines 1-20)
- The new module will export:
  - `pub fn encode_path_component(s: &str) -> String` -- encodes individual db/table name, does NOT preserve `/`
  - Path sanitization: strip leading `/`, reject/encode `..` components

### Consumers to Update

| File | Current Function | Import Change |
|------|-----------------|---------------|
| src/backup/collect.rs | `url_encode_path()` | Remove local fn, use `path_encoding::encode_path_component` |
| src/download/mod.rs | `url_encode()` | Remove local fn, use `path_encoding::encode_path_component` |
| src/upload/mod.rs | `url_encode_component()` | Remove local fn, use `path_encoding::encode_path_component` |
| src/restore/attach.rs | `url_encode()` | Remove `pub(crate)` fn, use `path_encoding::encode_path_component` |
| src/restore/mod.rs | `attach::url_encode()` | Change import to `path_encoding::encode_path_component` |

### Call Site Adjustment Notes

- **collect.rs:383-384**: Currently `url_encode_path(&db)` on full db name. Replace with `encode_path_component(&db)`.
- **download:522-523,847,852**: Currently `url_encode(&item.db)` for shadow paths and metadata dirs. Replace with component encoder.
- **upload:81-82,345-346,814-815,1084-1085**: Already uses component-level encoding (no `/`). Direct replacement.
- **restore:316-317,556-557,994-995**: Currently `url_encode(source_db)`. Replace with component encoder.
- **All callers pass individual db or table names** (not full paths), so component-level encoding (no `/` preservation) is correct for ALL call sites.
