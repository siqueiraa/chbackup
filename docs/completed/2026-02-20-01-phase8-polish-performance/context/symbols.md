# Type Verification Table

All types verified by reading source files directly.

## Manifest Types (src/manifest.rs)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `BackupManifest.manifest_version` | `u32` | `u32` | manifest.rs:22 |
| `BackupManifest.name` | `String` | `String` | manifest.rs:25 |
| `BackupManifest.timestamp` | `DateTime<Utc>` | `DateTime<Utc>` | manifest.rs:28 |
| `BackupManifest.compressed_size` | `u64` | `u64` | manifest.rs:44 |
| `BackupManifest.metadata_size` | `u64` | `u64` | manifest.rs:48 |
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | manifest.rs:65 |
| `BackupManifest.rbac` | `Option<RbacInfo>` | `Option<RbacInfo>` | manifest.rs:81 |
| `RbacInfo.path` | `String` | `String` | manifest.rs:189 |
| `PartInfo.name` | `String` | `String` | manifest.rs:123 |
| `PartInfo.size` | `u64` | `u64` | manifest.rs:127 |
| `PartInfo.backup_key` | `String` | `String` | manifest.rs:131 |
| `PartInfo.source` | `String` | `String` | manifest.rs:136 |

**Note**: BackupManifest does NOT currently have `rbac_size` or `config_size` fields. These must be added.

## List Types (src/list.rs)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `BackupSummary.name` | `String` | `String` | list.rs:45 |
| `BackupSummary.timestamp` | `Option<DateTime<Utc>>` | `Option<DateTime<Utc>>` | list.rs:47 |
| `BackupSummary.size` | `u64` | `u64` | list.rs:49 |
| `BackupSummary.compressed_size` | `u64` | `u64` | list.rs:51 |
| `BackupSummary.table_count` | `usize` | `usize` | list.rs:53 |
| `BackupSummary.metadata_size` | `u64` | `u64` | list.rs:55 |
| `BackupSummary.is_broken` | `bool` | `bool` | list.rs:57 |
| `BackupSummary.broken_reason` | `Option<String>` | `Option<String>` | list.rs:60 |

**Note**: BackupSummary does NOT currently have `rbac_size` or `config_size` fields.

## Server Route Types (src/server/routes.rs)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `ListResponse.rbac_size` | `u64` | `u64` | routes.rs:81 |
| `ListResponse.config_size` | `u64` | `u64` | routes.rs:82 |
| `ListResponse.metadata_size` | `u64` | `u64` | routes.rs:80 |
| `TablesParams.table` | `Option<String>` | `Option<String>` | routes.rs:90 |
| `TablesParams.all` | `Option<bool>` | `Option<bool>` | routes.rs:91 |
| `TablesParams.backup` | `Option<String>` | `Option<String>` | routes.rs:92 |
| `TablesResponseEntry.total_bytes` | `Option<u64>` | `Option<u64>` | routes.rs:103 |

## Storage Types (src/storage/s3.rs)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `S3Client.bucket` | `String` | `String` | s3.rs:46 |
| `S3Client.prefix` | `String` | `String` | s3.rs:48 |
| `RetryConfig.max_retries` | `u32` | `u32` | s3.rs:20 |
| `RetryConfig.base_delay_secs` | `u64` | `u64` | s3.rs:22 |
| `RetryConfig.jitter_factor` | `f64` | `f64` | s3.rs:24 |

## Upload Types (src/upload/mod.rs)

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `MULTIPART_THRESHOLD` | `u64` | `u64` (32 * 1024 * 1024) | upload/mod.rs:41 |
| `UploadWorkItem.s3_key` | `String` | `String` | upload/mod.rs:125 |

## Signal Types (tokio::signal)

| Item | Type | Location |
|---|---|---|
| `signal(SignalKind::hangup())` | `Signal` | server/mod.rs:217 |
| `tokio::signal::ctrl_c()` | Future | server/mod.rs:281 |
| `SignalKind::quit()` | `SignalKind` | Not yet used (must verify exists in tokio) |

## Crate Dependencies

| Crate | Version | Used For |
|---|---|---|
| `tokio` | 1 (features=["full"]) | Async runtime, signal handling |
| `tokio-util` | 0.7 (features=["codec"]) | CancellationToken, IO utilities |
| `aws-sdk-s3` | 1 | S3 operations |
| `walkdir` | 2 | Directory traversal |
| `lz4_flex` | 0.11 | LZ4 compression |
| `tar` | 0.4 | Tar archive creation |
| `axum` | 0.7 | HTTP API server |
| `arc-swap` | 1 | Hot-swappable server state |

## Types to Add

| New Type/Field | Target File | Type | Serde Attribute |
|---|---|---|---|
| `BackupManifest.rbac_size` | src/manifest.rs | `u64` | `#[serde(default)]` |
| `BackupManifest.config_size` | src/manifest.rs | `u64` | `#[serde(default)]` |
| `BackupSummary.rbac_size` | src/list.rs | `u64` | inherited from Serialize/Deserialize |
| `BackupSummary.config_size` | src/list.rs | `u64` | inherited from Serialize/Deserialize |
