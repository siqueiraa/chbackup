# Type Verification Table

## Issue 1: Path Traversal

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `url_encode_path(s)` return | `String` | `String` | grep @ collect.rs:29 |
| `url_encode(s)` in download | `String` | `String` | grep @ download/mod.rs:41 |
| `url_encode(s)` in restore | `String` | `String` | grep @ restore/attach.rs:844 |
| `url_encode_component(s)` in upload | `String` | `String` | grep @ upload/mod.rs:55 |
| `PartInfo.name` | `String` | `String` | manifest.rs (known) |
| `item.db` / `item.table` | `String` | `String` | download/mod.rs:80-82 |

## Issue 2: disable_cert_verification

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `S3Config.disable_cert_verification` | `bool` | `bool` | config.rs:284 |
| `S3Config.disable_ssl` | `bool` | `bool` | config.rs:280 |
| `aws_sdk_s3::config::Builder` | builder | `aws_sdk_s3::config::Builder` | s3.rs:141 |

## Issue 3: S3 Unit Tests

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `S3Client` struct fields | 7 String fields | `inner`, `bucket`, `prefix`, `storage_class`, `sse`, `sse_kms_key_id`, `acl` | s3.rs:43-57 |
| `mock_s3_client()` return | `S3Client` | `S3Client` | s3.rs:1520 |

## Issue 4: disable_ssl

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `S3Config.disable_ssl` | `bool` | `bool` | config.rs:280 |
| `config.endpoint` | `String` | `String` | config.rs (S3Config) |

## Issue 5: check_parts_columns

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `config.clickhouse.check_parts_columns` | `bool` | `bool` | grep in config.rs |
| `ColumnInconsistency.database` | `String` | `String` | client.rs:72 |
| `ColumnInconsistency.table` | `String` | `String` | client.rs:73 |
| `ColumnInconsistency.column` | `String` | `String` | client.rs:74 |
| `ColumnInconsistency.types` | `Vec<String>` | `Vec<String>` | client.rs:75 |
| `filter_benign_type_drift()` return | `Vec<ColumnInconsistency>` | `Vec<ColumnInconsistency>` | backup/mod.rs:803 |

## Issue 6: --env format

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `Config::load()` signature | `(path, cli_env_overrides: &[String])` | confirmed | config.rs:843 |
| `apply_cli_env_overrides()` | `(&mut self, overrides: &[String])` | confirmed | config.rs:1100 |
| `set_field()` match arms | dot-notation keys | ~80 arms, dot-notation only | config.rs:1112-1360 |

## Issue 7: DRY url_encode

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `url_encode_path()` in collect.rs | `pub fn(s: &str) -> String` | confirmed | collect.rs:29 |
| `url_encode()` in download | `fn(s: &str) -> String` (private) | confirmed | download/mod.rs:41 |
| `url_encode_component()` in upload | `fn(s: &str) -> String` (private) | confirmed | upload/mod.rs:55 |
| `url_encode()` in attach.rs | `pub(crate) fn(s: &str) -> String` | confirmed | restore/attach.rs:844 |
