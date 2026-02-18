# Plan: Phase 0 — Skeleton

**Goal**: Project compiles, CLI parses all commands/flags, connects to ClickHouse and S3, handles config and logging. Does nothing useful yet, but every subsequent phase builds on this foundation.

**Design sections consumed**: §2 (Commands), §11.3 (crate choices), §11.4 (Logging), §12 (Configuration)

---

## Architecture Assumptions (VALIDATED)

1. **Greenfield project** — No existing Rust code. All files created from scratch.
2. **Crate choices from §11.3** — `clickhouse` (clickhouse-rs), `aws-sdk-s3`, `tokio`, `tokio-util`, `clap`, `serde_yaml`, `thiserror`, `anyhow`, `tracing`, `walkdir`. (`tokio-util` and `walkdir` added for Phase 1 but declared now per §11.3.)
3. **Config structure from §12** — ~106 params across 7 sections: general (14), clickhouse (37), s3 (20), backup (13), retention (2), watch (7), api (13). The design doc says "~40" but actual count from the YAML block is ~106.
4. **CLI commands from §2** — 15 subcommands with full flag sets defined upfront. Each command's flags explicitly listed per the flag reference table.
5. **PID lock from §2** — Three tiers: backup-scoped, global, none. JSON lock file with PID/command/timestamp.
6. **Logging from §11.4** — Two modes: human-readable (CLI) and JSON (server/`--log-format=json`). Mode selected by `general.log_format` config field OR server command. `tracing` + `tracing-subscriber`.

---

## Risk Assessment

| Risk | Impact | Mitigation |
|------|--------|------------|
| `clickhouse` crate API mismatch | Medium | Task 7 is thin wrapper; verify crate docs during implementation |
| `aws-sdk-s3` config complexity | Medium | Task 8 handles force_path_style, assume_role, endpoint; test against MinIO |
| Config env overlay edge cases | Low | Task 3 has 5 unit tests covering overlay behavior |
| PID lock race condition | Low | Task 6 uses atomic file creation; 3 unit tests |

---

## Expected Runtime Logs

Phase 0 is CLI-only (no long-running server). Verification is via command output, not log patterns.

| Binary | Pattern | When |
|--------|---------|------|
| chbackup | `general:` | `default-config` prints YAML with general section |
| chbackup | `Usage: chbackup create` | `create --help` shows usage |
| chbackup | `Connecting to ClickHouse` | `list` attempts ClickHouse connection |
| chbackup | `Connecting to S3` | `list` attempts S3 connection |
| chbackup | `not implemented yet` | Stub commands log unimplemented status |

---

## Known Related Issues (Out of Scope)

- **Phase 1 commands** (create, upload, download, restore) — stub implementations only in Phase 0
- **Progress bars** (`indicatif`) — deferred to Phase 1 when upload/download have real work
- **Server/API mode** (`axum`/`warp`) — deferred to Phase 3
- **Watch mode state machine** — deferred to Phase 3
- **Multipart upload** — deferred to Phase 1
- **Compression streaming** — `lz4_flex` added as dependency but not used until Phase 1

---

## Tasks

### Group 1: Project Foundation

#### Task 1: Cargo workspace and error types

**File**: `Cargo.toml`, `src/main.rs`, `src/error.rs`

Create the Cargo workspace with all dependencies from §11.3 and define error types.

**Steps**:
1. Create `Cargo.toml` with dependencies:
   - `clap` (derive feature) for CLI
   - `serde`, `serde_yaml` for config
   - `thiserror` for error types
   - `anyhow` for binary-level error propagation
   - `tokio` (full features) for async runtime
   - `tracing`, `tracing-subscriber` (json feature) for logging
   - `clickhouse` (clickhouse-rs crate) for ClickHouse
   - `aws-sdk-s3`, `aws-config` for S3
   - `lz4_flex` for compression (future phases)
   - `tokio-util` (codec feature) for async stream transforms (future phases, per §11.3)
   - `walkdir` for directory walks (future phases, per §11.3)
   - `chrono` for timestamps in lock files
   - `serde_json` for lock file serialization
2. Create `src/error.rs` with thiserror enum:
   - `ClickHouseError(String)`
   - `S3Error(String)`
   - `ConfigError(String)`
   - `LockError(String)`
   - `IoError(#[from] std::io::Error)`
3. Create minimal `src/main.rs` with `#[tokio::main]` entry point

**Test**: `cargo check` compiles without errors

**Acceptance**:
- [ ] `Cargo.toml` has all required dependencies
- [ ] `src/error.rs` defines error enum with 5 variants
- [ ] `cargo check` passes

---

#### Task 2: CLI skeleton with all commands and flags

**File**: `src/cli.rs`, `src/main.rs`

Define all commands from §2 as clap subcommands with all flags from the flag reference table.

**Steps**:
1. Create `src/cli.rs` with `#[derive(Parser)]` struct `Cli`
2. Global flags: `-c/--config` (default `/etc/chbackup/config.yml`, env `CHBACKUP_CONFIG`), `--env` (Vec<String>)
3. Subcommands enum with all commands from §2:
   - `Create` with flags: `-t/--tables`, `--partitions`, `--diff-from`, `--skip-projections`, `--schema`, `--rbac`, `--configs`, `--named-collections`, `--skip-check-parts-columns`, `--resume`, optional `backup_name`
   - `Upload` with flags: `--delete-local`, `--diff-from-remote`, `--resume`, optional `backup_name`
   - `Download` with flags: `--hardlink-exists-files`, `--resume`, optional `backup_name`
   - `Restore` with flags: `-t/--tables`, `--as` (rename), `-m/--database-mapping`, `--partitions`, `--schema`, `--data-only`, `--rm/--drop`, `--resume`, `--rbac`, `--configs`, `--named-collections`, `--skip-empty-tables`, optional `backup_name`
   - `CreateRemote` with flags: `-t/--tables`, `--diff-from-remote`, `--delete-source`, `--rbac`, `--configs`, `--named-collections`, `--skip-check-parts-columns`, `--skip-projections`, `--resume`, optional `backup_name` (NOTE: no `--diff-from`, `--partitions`, or `--schema` — those are create-only per §2 flag table)
   - `RestoreRemote` with flags: `-t/--tables`, `--as` (rename), `-m/--database-mapping`, `--rm/--drop`, `--rbac`, `--configs`, `--named-collections`, `--skip-empty-tables`, `--resume`, optional `backup_name` (NOTE: no `--partitions`, `--schema`, `--data-only` — those are restore-only per §2 flag table)
   - `List` with optional positional `local|remote`
   - `Tables` with flags: `-t/--tables`, `--all`, `--remote-backup`
   - `Delete` with positional `local|remote`, optional `backup_name`
   - `Clean` with `--name`
   - `CleanBroken` with positional `local|remote`
   - `DefaultConfig` (no flags)
   - `PrintConfig` (no flags)
   - `Watch` with flags: `--watch-interval`, `--full-interval`, `--name-template`, `-t/--tables`
   - `Server` with flag: `--watch`
4. Wire `Cli::parse()` in `main.rs` with match on subcommands (stub implementations)

**Test**: `cargo run -- create --help` shows all flags

**Acceptance**:
- [ ] All 15 subcommands parse correctly
- [ ] `--help` for each command shows correct flags
- [ ] Global `--config` and `--env` flags work

---

### Group 2: Configuration

#### Task 3: Configuration loader with env overlay

**File**: `src/config.rs`

Implement the full config from §12 (~106 params across 7 sections) with YAML deserialization and environment variable overlay.

**Steps**:
1. Define config structs matching §12 exactly:
   - `Config` (top-level): `general`, `clickhouse`, `s3`, `backup`, `retention`, `watch`, `api`
   - `GeneralConfig`: all fields from §12 general section
   - `ClickHouseConfig`: all fields from §12 clickhouse section
   - `S3Config`: all fields from §12 s3 section
   - `BackupConfig`: all fields from §12 backup section
   - `RetentionConfig`: `backups_to_keep_local`, `backups_to_keep_remote`
   - `WatchConfig`: all fields from §12 watch section
   - `ApiConfig`: all fields from §12 api section
2. Implement `Default` for all config structs with values from §12
3. `Config::load(path: &Path)` — deserialize YAML, apply env var overlay
4. Env overlay: `S3_BUCKET` → `s3.bucket`, `CLICKHOUSE_HOST` → `clickhouse.host`, etc.
5. CLI `--env` flags override env vars: parse `KEY=VALUE` pairs
6. Validation: `full_interval > watch_interval`, concurrency > 0, etc.
7. `Config::default_yaml()` — serialize default config to YAML string

**Test**: Write `tests/config_test.rs`:
- `test_default_config_serializes` — default config serializes to valid YAML
- `test_config_from_yaml` — parse a minimal YAML config
- `test_env_overlay` — env var overrides config value
- `test_cli_env_override` — `--env` overrides env var
- `test_validation_full_interval` — full_interval <= watch_interval fails

**Acceptance**:
- [ ] All ~106 config params have serde annotations and defaults
- [ ] Env overlay works (env → config field mapping)
- [ ] Validation catches invalid config
- [ ] 5 unit tests pass

---

#### Task 4: Wire default-config and print-config commands

**File**: `src/main.rs`

**Steps**:
1. `DefaultConfig` handler: print `Config::default_yaml()` to stdout
2. `PrintConfig` handler: load config from file (with env overlay), serialize to YAML, print
3. Handle config file not found gracefully (use defaults if no file exists)

**Test**: `cargo run -- default-config` outputs valid YAML with all config sections

**Acceptance**:
- [ ] `default-config` prints complete valid YAML
- [ ] `print-config` loads and resolves config correctly

---

### Group 3: Logging and PID Lock

#### Task 5: Logging setup

**File**: `src/logging.rs`

**Steps**:
1. `init_logging(config: &GeneralConfig, is_server: bool)`:
   - Select JSON mode if `config.log_format == "json"` OR `is_server == true` (per §11.4: "Server/JSON mode when running as `server` or `--log-format=json`")
   - CLI/text mode (default): human-readable format with colors via `tracing_subscriber::fmt`
   - JSON mode: structured JSON lines via `tracing_subscriber::fmt::format::json()`
   - Log level from config `log_level` field, overridden by `RUST_LOG` env var
2. Wire logging init in `main.rs` before any command execution

**Test**: `cargo run -- default-config 2>&1` shows no logging errors

**Acceptance**:
- [ ] CLI mode uses human-readable format when `log_format == "text"`
- [ ] JSON mode used when `log_format == "json"` OR server command
- [ ] `RUST_LOG` overrides config log level

---

#### Task 6: PID lock

**File**: `src/lock.rs`

Three-tier lock from §2.

**Steps**:
1. `PidLock` struct with `acquire(path: &Path) -> Result<PidLock>` and `Drop` impl
2. Lock file contains: PID, command name, timestamp (JSON via serde_json)
3. On acquire: check if existing PID is alive (`kill(pid, 0)`), if alive return `LockError`
4. On drop: remove lock file
5. `LockScope` enum: `Backup(String)`, `Global`, `None`
6. `lock_for_command(command, backup_name) -> LockScope` mapping per §2:
   - `Backup(name)`: create, upload, download, restore, create_remote, restore_remote
   - `Global`: clean, clean_broken, delete
   - `None`: list, tables, default-config, print-config, watch, server

**Test**: Write unit tests:
- `test_acquire_release` — lock acquired, dropped, file removed
- `test_double_acquire_fails` — second acquire on same path fails
- `test_stale_lock_overridden` — dead PID lock gets overridden

**Acceptance**:
- [ ] Lock file created with PID/command/timestamp
- [ ] Concurrent lock on same path fails
- [ ] Stale lock (dead PID) gets overridden
- [ ] Lock file cleaned up on Drop

---

### Group 4: External Connections

#### Task 7: ClickHouse client wrapper

**File**: `src/clickhouse/mod.rs`, `src/clickhouse/client.rs`

**Steps**:
1. `ChClient` struct wrapping `clickhouse::Client` from clickhouse-rs
2. `ChClient::new(config: &ClickHouseConfig) -> Result<ChClient>`: build URL, set credentials, configure TLS
3. `ChClient::ping(&self) -> Result<()>` — execute `SELECT 1`
4. Wire into `list` command stub

**Test**: Compilation test only (real ClickHouse needed for integration)

**Acceptance**:
- [ ] `ChClient` wraps clickhouse-rs with config-driven setup
- [ ] `ping()` executes `SELECT 1`

---

#### Task 8: S3 client wrapper

**File**: `src/storage/mod.rs`, `src/storage/s3.rs`

**Steps**:
1. `S3Client` struct wrapping `aws_sdk_s3::Client`
2. `S3Client::new(config: &S3Config) -> Result<S3Client>`: build from aws-config with region, endpoint, credentials, force_path_style, assume_role_arn
3. `S3Client::ping(&self) -> Result<()>` — `ListObjectsV2` with max_keys=1
4. Wire into `list` command stub

**Test**: Compilation test only (real S3 needed for integration)

**Acceptance**:
- [ ] `S3Client` wraps aws-sdk-s3 with config-driven setup
- [ ] `ping()` lists objects to verify connectivity

---

### Group 5: Integration and config.example.yml

#### Task 9: config.example.yml and final wiring

**File**: `config.example.yml`, `src/main.rs`

**Steps**:
1. Generate `config.example.yml` from `Config::default_yaml()` with section comments
2. Wire all commands in `main.rs`: Parse CLI → Load config → Init logging → Acquire lock → Execute command → Release lock
3. `list` command stub: connect to ClickHouse and S3, print connection status
4. All other commands: print "not implemented yet" with proper logging

**Test**:
- `cargo build` — zero warnings
- `cargo run -- default-config` — prints valid YAML
- `cargo run -- create --help` — shows all flags
- `cargo test` — all unit tests pass

**Acceptance**:
- [ ] `config.example.yml` exists with all params documented
- [ ] All commands route through config → logging → lock flow
- [ ] Zero compiler warnings
- [ ] All unit tests pass

---

## Dependency Groups

```
Group 1: [Task 1, Task 2]     → no dependencies
Group 2: [Task 3, Task 4]     → depends on Group 1
Group 3: [Task 5, Task 6]     → depends on Group 1
Group 4: [Task 7, Task 8]     → depends on Group 2
Group 5: [Task 9]             → depends on Groups 2, 3, 4
```

---

## Definition of Done (from roadmap)

```bash
cargo build --release                    # compiles, zero warnings
cargo test                               # all unit tests pass
cargo run -- default-config              # prints valid YAML
cargo run -- create --help               # shows all flags
cargo run -- list                        # connects to CH + S3 (may fail without services, but code path runs)
```

Note: Roadmap DoD specifies `--target x86_64-unknown-linux-musl` for static binary. This is Linux-only and will be verified in CI/Docker, not on macOS dev machines.
