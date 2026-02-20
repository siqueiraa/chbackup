# Pattern Discovery — Phase 1

No global patterns directory exists (`docs/patterns/` not found). Patterns discovered from Phase 0 codebase.

## 1. Module Organization Pattern

**Source:** `src/lib.rs`, `src/clickhouse/mod.rs`, `src/storage/mod.rs`

Pattern: Each logical domain gets a directory with `mod.rs` + specific files. `mod.rs` re-exports the public API.

```
src/clickhouse/
  mod.rs       — `pub mod client; pub use client::ChClient;`
  client.rs    — implementation

src/storage/
  mod.rs       — `pub mod s3; pub use s3::S3Client;`
  s3.rs        — implementation
```

**lib.rs** declares all top-level modules:
```rust
pub mod clickhouse;
pub mod config;
pub mod error;
pub mod lock;
pub mod logging;
pub mod storage;
```

**Phase 1 must follow:** New modules (`backup/`, `upload/`, `download/`, `restore/`, `manifest.rs`, `list.rs`, `table_filter.rs`) must be declared in `lib.rs` and follow the same `mod.rs` + implementation files pattern.

## 2. Client Wrapper Pattern

**Source:** `src/clickhouse/client.rs`, `src/storage/s3.rs`

Pattern: Thin wrapper struct around external crate client with:
1. `new(config: &XxxConfig) -> Result<Self>` constructor
2. `ping() -> Result<()>` connectivity test
3. `inner() -> &InnerClient` accessor for direct access
4. Store config fields needed for diagnostics (host, port, bucket, prefix)
5. Use `tracing::info!` for key lifecycle events
6. Config comes from the config module's typed structs
7. Unit tests verify construction with default config

```rust
pub struct ChClient {
    inner: clickhouse::Client,
    host: String,
    port: u16,
}

impl ChClient {
    pub fn new(config: &ClickHouseConfig) -> Result<Self> { ... }
    pub async fn ping(&self) -> Result<()> { ... }
    pub fn inner(&self) -> &clickhouse::Client { &self.inner }
}
```

**Phase 1 must follow:** ChClient needs new query methods. Follow the same pattern of taking config references, returning `anyhow::Result`, and logging with tracing.

## 3. Error Handling Pattern

**Source:** `src/error.rs`, all modules

Two-level error approach:
- `ChBackupError` (thiserror enum) for typed, matchable errors
- `anyhow::Result` at the binary boundary and in most functions

```rust
#[derive(Debug, Error)]
pub enum ChBackupError {
    #[error("ClickHouse error: {0}")]
    ClickHouseError(String),
    #[error("S3 error: {0}")]
    S3Error(String),
    #[error("Configuration error: {0}")]
    ConfigError(String),
    #[error("Lock error: {0}")]
    LockError(String),
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),
}
```

**Phase 1 must follow:** Add new error variants for backup/restore operations (e.g., `BackupError`, `RestoreError`, `ManifestError`). Keep using `anyhow::Context` for adding context to errors.

## 4. Config Pattern

**Source:** `src/config.rs`

- Flat struct hierarchy: `Config { general, clickhouse, s3, backup, ... }`
- `#[derive(Debug, Clone, Default, Serialize, Deserialize)]` on all config structs
- `#[serde(default = "default_xxx")]` for every field
- Explicit `Default` impl that calls the same default functions
- `validate()` method for cross-field validation
- `apply_env_overlay()` for environment variable overrides

**Phase 1 must follow:** Config already has `backup.allow_empty_backups`, `backup.compression`, `clickhouse.ignore_not_exists_error_during_freeze`, `clickhouse.log_sql_queries`, `clickhouse.sync_replicated_tables`, `clickhouse.data_path` etc. Use these existing config fields, do NOT duplicate them.

## 5. CLI Command Pattern

**Source:** `src/cli.rs`, `src/main.rs`

- `clap` derive API with `#[derive(Subcommand)]`
- All flags defined on the `Command` enum variants
- `main.rs` pattern: match on command, create clients, execute
- Lock acquisition before command execution
- Early return for non-operational commands (default-config, print-config)

**Phase 1 must follow:** Commands are already defined as enum variants with all flags. Implementation goes into new modules called from `main.rs` match arms.

## 6. Testing Pattern

**Source:** `tests/config_test.rs`, `src/clickhouse/client.rs`, `src/lock.rs`

- Integration tests in `tests/` directory
- Unit tests as `#[cfg(test)] mod tests` in source files
- Use `tempfile` crate for temporary directories/files
- `ENV_LOCK` mutex for tests that modify env vars
- Assert both success and error cases

**Phase 1 must follow:** Unit tests for pure logic (CRC64, manifest serialization, table filter, part name parsing). Integration tests requiring real ClickHouse + S3 in `tests/` directory.

## 7. ClickHouse Client Crate

**Source:** `Cargo.toml`, `src/clickhouse/client.rs`

The project uses `clickhouse` crate (clickhouse-rs), NOT the native protocol. This crate uses the **HTTP interface** (port 8123 by default, not 9000).

Key APIs:
- `Client::default().with_url(url).with_user(user).with_password(pass)` — construction
- `client.query("SQL").execute().await` — execute without results
- `client.query("SQL").fetch_all::<Row>().await` — fetch rows (requires Row derive)

Note: Default port in config is 9000 but the HTTP interface is used. The config should be checked for port correctness. FREEZE/UNFREEZE, SYSTEM commands are executed via SQL queries through the HTTP interface.

## 8. S3 Client Operations

**Source:** `src/storage/s3.rs`, `Cargo.toml`

Uses `aws-sdk-s3` with `aws-config`. Key operations for Phase 1:
- `put_object().bucket().key().body(ByteStream).send()` — upload
- `get_object().bucket().key().send()` — download (body is streaming)
- `list_objects_v2().bucket().prefix().send()` — list
- `delete_object().bucket().key().send()` — delete single
- `delete_objects().bucket().delete(Delete::builder().objects(...))` — batch delete

The S3Client stores `bucket` and `prefix` from config, with accessor methods `bucket()` and `prefix()`.
