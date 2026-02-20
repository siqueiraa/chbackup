# Type Verification

## Phase 3e Symbol Analysis

Phase 3e is **infrastructure-only** (Docker, CI, K8s manifests). Type verification focuses on the Rust CLI interface and configuration structure that Docker/CI files reference.

### CLI Interface (verified in src/cli.rs)

| Symbol | Location | Verified |
|--------|----------|----------|
| `Command::Server { watch: bool }` | `src/cli.rs:328-332` | YES -- `--watch` flag exists on Server subcommand |
| `Command::Watch { ... }` | `src/cli.rs:309-325` | YES -- standalone watch command with interval overrides |
| `Cli { config: String, ... }` | `src/cli.rs:7-24` | YES -- `-c` flag, default `/etc/chbackup/config.yml`, env `CHBACKUP_CONFIG` |

### Configuration Structure (verified in src/config.rs)

| Symbol | Location | Verified |
|--------|----------|----------|
| `Config` | `src/config.rs:7-29` | YES -- 7 sections: general, clickhouse, s3, backup, retention, watch, api |
| `WatchConfig.enabled` | `src/config.rs` | YES -- `watch.enabled: false` default |
| `ApiConfig.listen` | `src/config.rs` | YES -- default `localhost:7171` |
| `ApiConfig.watch_is_main_process` | `src/config.rs:480` | YES -- default `false` |
| `ApiConfig.enable_metrics` | `src/config.rs` | YES -- for Prometheus /metrics endpoint |
| `ApiConfig.create_integration_tables` | `src/config.rs` | YES -- for system.backup_list/backup_actions |

### Server Module (verified in src/server/mod.rs)

| Symbol | Location | Verified |
|--------|----------|----------|
| `start_server(config, ch, s3, watch, config_path)` | `src/server/mod.rs:117-309` | YES -- entry point for server mode |
| `build_router(state) -> Router` | `src/server/mod.rs:41-105` | YES -- all API routes |

### Environment Variable Overlay (verified in config.rs and config.example.yml)

| Env Var | Config Path | Verified |
|---------|-------------|----------|
| `S3_BUCKET` | `s3.bucket` | YES |
| `S3_ACCESS_KEY` | `s3.access_key` | YES |
| `S3_SECRET_KEY` | `s3.secret_key` | YES |
| `S3_REGION` | `s3.region` | YES |
| `S3_ENDPOINT` | `s3.endpoint` | YES |
| `CHBACKUP_CONFIG` | CLI `-c` flag | YES |
| `WATCH_INTERVAL` | `watch.watch_interval` | Needs verification |
| `FULL_INTERVAL` | `watch.full_interval` | Needs verification |

### Environment Variable Mapping (verified in src/config.rs)

The env overlay in `config.rs` uses a flat key mapping pattern. Need to verify exact env var names for watch settings.

| Config Key | Env Var Pattern | Status |
|------------|----------------|--------|
| `s3.bucket` | `S3_BUCKET` | VERIFIED in config.rs |
| `s3.access_key` | `S3_ACCESS_KEY` | VERIFIED in config.rs |
| `watch.watch_interval` | `WATCH_INTERVAL` | USED in design doc 10.9, needs code verification |
| `watch.full_interval` | `FULL_INTERVAL` | USED in design doc 10.9, needs code verification |

### Build Target

| Component | Value | Source |
|-----------|-------|--------|
| Rust target triple | `x86_64-unknown-linux-musl` | Design doc 1.2, Dockerfile.test |
| Alpine base (runtime) | `alpine:3.21` | Design doc 1.2 |
| Alpine base (builder) | `rust:1.82-alpine` | Design doc 1.2 |
| ClickHouse uid/gid | `101` | Design doc 1.2 |
