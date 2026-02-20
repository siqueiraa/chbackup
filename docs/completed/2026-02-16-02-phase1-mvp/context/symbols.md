# Type Verification — Phase 1

## Existing Types (Verified from Phase 0 codebase)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `Config` | struct | `pub struct Config { general, clickhouse, s3, backup, retention, watch, api }` | src/config.rs:8 |
| `Config.clickhouse` | ClickHouseConfig | `pub clickhouse: ClickHouseConfig` | src/config.rs:14 |
| `Config.s3` | S3Config | `pub s3: S3Config` | src/config.rs:17 |
| `Config.backup` | BackupConfig | `pub backup: BackupConfig` | src/config.rs:20 |
| `ClickHouseConfig.data_path` | String | `pub data_path: String` (default: "/var/lib/clickhouse") | src/config.rs:112 |
| `ClickHouseConfig.ignore_not_exists_error_during_freeze` | bool | `pub ignore_not_exists_error_during_freeze: bool` (default: true) | src/config.rs:174 |
| `ClickHouseConfig.sync_replicated_tables` | bool | `pub sync_replicated_tables: bool` (default: true) | src/config.rs:139 |
| `ClickHouseConfig.log_sql_queries` | bool | `pub log_sql_queries: bool` (default: true) | src/config.rs:170 |
| `ClickHouseConfig.max_connections` | u32 | `pub max_connections: u32` (default: 1) | src/config.rs:166 |
| `ClickHouseConfig.mutation_wait_timeout` | String | `pub mutation_wait_timeout: String` (default: "5m") | src/config.rs:149 |
| `ClickHouseConfig.backup_mutations` | bool | `pub backup_mutations: bool` (default: true) | src/config.rs:183 |
| `BackupConfig.allow_empty_backups` | bool | `pub allow_empty_backups: bool` (default: false) | src/config.rs:332 |
| `BackupConfig.compression` | String | `pub compression: String` (default: "lz4") | src/config.rs:336 |
| `BackupConfig.compression_level` | u32 | `pub compression_level: u32` (default: 1) | src/config.rs:339 |
| `S3Config.bucket` | String | `pub bucket: String` | src/config.rs:246 |
| `S3Config.prefix` | String | `pub prefix: String` | src/config.rs:257 |
| `S3Config.storage_class` | String | `pub storage_class: String` (default: "STANDARD") | src/config.rs:286 |
| `S3Config.acl` | String | `pub acl: String` (default: "") | src/config.rs:282 |
| `S3Config.sse` | String | `pub sse: String` (default: "") | src/config.rs:290 |
| `S3Config.sse_kms_key_id` | String | `pub sse_kms_key_id: String` (default: "") | src/config.rs:294 |
| `ChClient.inner` | clickhouse::Client | `inner: clickhouse::Client` | src/clickhouse/client.rs:13 |
| `ChClient` | struct | `pub struct ChClient { inner, host, port }` | src/clickhouse/client.rs:12-17 |
| `S3Client.inner` | aws_sdk_s3::Client | `inner: aws_sdk_s3::Client` | src/storage/s3.rs:13 |
| `S3Client.bucket` | String | `bucket: String` | src/storage/s3.rs:15 |
| `S3Client.prefix` | String | `prefix: String` | src/storage/s3.rs:17 |
| `ChBackupError` | enum | 5 variants: ClickHouseError, S3Error, ConfigError, LockError, IoError | src/error.rs:5 |
| `Location` | enum | `Local, Remote` | src/cli.rs:29 |
| `Command` | enum | 15 variants (all CLI commands) | src/cli.rs:34 |
| `Cli.config` | String | `pub config: String` | src/cli.rs:16 |
| `Cli.env_overrides` | Vec<String> | `pub env_overrides: Vec<String>` | src/cli.rs:20 |

## New Types to be Defined in Phase 1

| Type Name | Kind | Key Fields | Design Section |
|---|---|---|---|
| `BackupManifest` | struct | manifest_version, name, timestamp, clickhouse_version, chbackup_version, data_format, compressed_size, metadata_size, disks, disk_types, tables, databases, functions, named_collections, rbac | §7.1 |
| `TableManifest` | struct | ddl, uuid, engine, total_bytes, parts, pending_mutations, metadata_only, dependencies | §7.1 |
| `PartInfo` | struct | name, size, backup_key, source, checksum_crc64, s3_objects (Option) | §7.1 |
| `DatabaseInfo` | struct | name, ddl | §7.1 |
| `PartSource` | enum | Uploaded, Carried(String) | §3.5 |
| `FreezeGuard` | struct | ch_client, freeze_name, db, table (Drop impl for UNFREEZE) | §3.4 |
| `TableFilter` | struct | pattern (for glob matching db.table) | §2 |

## Crate API Verification

### `clickhouse` crate (v0.13)
- `Client::default()` — create client
- `.with_url(&str)` — set URL
- `.with_user(&str)` — set username
- `.with_password(&str)` — set password
- `.query(&str).execute().await` — execute DDL/DML (no results)
- `.query(&str).fetch_all::<T>().await` — fetch typed rows
- `.query(&str).fetch_one::<T>().await` — fetch single row
- Row types use `#[derive(clickhouse::Row, serde::Deserialize)]`

### `aws-sdk-s3` crate (v1)
- `client.put_object().bucket().key().body(ByteStream::from(bytes)).send().await`
- `client.get_object().bucket().key().send().await` — returns `GetObjectOutput` with `.body` as `ByteStream`
- `client.list_objects_v2().bucket().prefix().send().await` — returns `ListObjectsV2Output`
- `client.delete_object().bucket().key().send().await`
- `client.delete_objects().bucket().delete(Delete::builder().objects(ObjectIdentifier::builder().key(k).build()).build()).send().await`
- `ByteStream::from(Vec<u8>)` or `ByteStream::from_path(path).await`

### `lz4_flex` crate (v0.11)
- `lz4_flex::frame::FrameEncoder::new(writer)` — streaming compression
- `lz4_flex::frame::FrameDecoder::new(reader)` — streaming decompression

### `walkdir` crate (v2)
- `WalkDir::new(path)` — iterate directory tree
- Returns `DirEntry` with `.path()`, `.file_type()`, `.metadata()`

### `crc64fast` (not in Cargo.toml yet — needs to be added)
- `crc64fast::Digest::new()` — create CRC64 hasher
- `.write(bytes)` — feed data
- `.sum64()` — get checksum
- Note: Need to verify if this crate exists or if we need an alternative

## Filesystem APIs (std library)
- `std::os::unix::fs::symlink` — not needed, use `hard_link`
- `std::fs::hard_link(src, dst)` — hardlink (returns `io::Error` with `ErrorKind::CrossesDevices` for EXDEV)
- `std::fs::copy(src, dst)` — fallback copy
- `std::fs::create_dir_all(path)` — recursive mkdir
- `std::fs::remove_dir_all(path)` — recursive delete
- `std::fs::read_to_string(path)` — read file contents
- `std::fs::write(path, contents)` — write file contents
