# Symbols

**Plan:** 2026-02-20-01-go-parity-gaps

## Key Types (verified via source grep)

| Symbol | File | Line (approx) | Verified |
|--------|------|---------------|----------|
| `Config` | `src/config.rs` | 7 | YES |
| `GeneralConfig` | `src/config.rs` | 35 | YES |
| `ClickHouseConfig` | `src/config.rs` | 97 | YES |
| `S3Config` | `src/config.rs` | ~200 | YES |
| `BackupConfig` | `src/config.rs` | ~300 | YES |
| `RetentionConfig` | `src/config.rs` | ~400 | YES |
| `WatchConfig` | `src/config.rs` | ~420 | YES |
| `ApiConfig` | `src/config.rs` | ~450 | YES |
| `Cli` | `src/cli.rs` | 22 | YES |
| `Command` | `src/cli.rs` | 50 | YES |
| `Location` | `src/cli.rs` | 43 | YES |
| `ListFormat` | `src/cli.rs` | 5 | YES |
| `S3Client` | `src/storage/s3.rs` | ~30 | YES |
| `ChClient` | `src/clickhouse/client.rs` | ~10 | YES |
| `BackupManifest` | `src/manifest.rs` | ~10 | YES |
| `TableManifest` | `src/manifest.rs` | ~50 | YES |
| `BackupSummary` | `src/list.rs` | ~20 | YES |
| `AppState` | `src/server/state.rs` | ~10 | YES |

## Type Verification Table

| Type Used in PLAN.md | Actual Type | Location | Verified |
|---------------------|-------------|----------|----------|
| `GeneralConfig.backups_to_keep_remote` | `i32` | `src/config.rs` | YES - default fn `default_backups_to_keep_remote_general` |
| `GeneralConfig.retries_jitter` | `u32` | `src/config.rs` | YES |
| `ClickHouseConfig.sync_replicated_tables` | `bool` | `src/config.rs` | YES |
| `BackupConfig.compression` | `String` | `src/config.rs` | YES |
| `ApiConfig.create_integration_tables` | `bool` | `src/config.rs` | YES |
| `S3Client.assume_role_arn` | via `S3Config.assume_role_arn` | `src/config.rs` | YES |
| `Command::Upload` | enum variant | `src/cli.rs` | YES |
| `Command::Download` | enum variant | `src/cli.rs` | YES |
| `Command::Restore` | enum variant | `src/cli.rs` | YES |
| `apply_env_overlay` | `fn(&mut Config)` | `src/config.rs` | YES |
