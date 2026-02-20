# Diagnostics Report -- Phase 3d Watch Mode

## Compiler State

**Date:** 2026-02-18
**Command:** `cargo check`
**Result:** PASS -- zero errors, zero warnings

```
Checking chbackup v0.1.0 (/Users/rafael.siqueira/dev/personal/chbackup)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 31.87s
```

## Existing Stub Endpoints (Phase 3d)

The following endpoints are currently stubs returning 501 Not Implemented:

| Endpoint | Location | Note |
|----------|----------|------|
| `POST /api/v1/reload` | `src/server/routes.rs:1096` | Config hot-reload |
| `POST /api/v1/restart` | `src/server/routes.rs:1101` | Server restart |
| `POST /api/v1/watch/start` | `src/server/routes.rs:1111` | Start watch loop |
| `POST /api/v1/watch/stop` | `src/server/routes.rs:1116` | Stop watch loop |
| `GET /api/v1/watch/status` | `src/server/routes.rs:1121` | Watch loop status |

## Existing Watch CLI Stub

`Command::Watch` in `main.rs:386-388` currently logs "watch: not implemented in Phase 1".

`Command::Server { watch: true }` in `main.rs:390-399` warns "--watch flag is not yet implemented (Phase 3d)" and starts server without watch.

## Config Already Defined

The `WatchConfig` struct is already defined in `src/config.rs:392-420`:
- `enabled: bool` (default: false)
- `watch_interval: String` (default: "1h")
- `full_interval: String` (default: "24h")
- `name_template: String` (default: "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}")
- `max_consecutive_errors: u32` (default: 5)
- `retry_interval: String` (default: "5m")
- `delete_local_after_upload: bool` (default: true)

**Missing from WatchConfig:** The design doc section 10.3 shows `tables: "*.*"` in the watch config, but the WatchConfig struct does NOT have a `tables` field. The CLI `Watch` command has `--tables/-t`, but there is no corresponding config-level field. This needs to be added.

## Config Validation Already Wired

In `config.rs:1184-1196`, when `watch.enabled` is true, the validator already checks that `full_interval > watch_interval` by parsing both durations.

## Duration Parser (Private)

`parse_duration_secs()` at `config.rs:1248` is private (`fn`, not `pub fn`). The watch module will need access to this function for parsing intervals. It needs to be made `pub`.

## Metrics Already Registered

The following watch-related metrics are already registered in `src/server/metrics.rs:151-174` but default to 0:
- `chbackup_watch_state` (IntGauge)
- `chbackup_watch_last_full_timestamp` (Gauge)
- `chbackup_watch_last_incremental_timestamp` (Gauge)
- `chbackup_watch_consecutive_errors` (IntGauge)

## system.macros Query NOT Implemented

The name template supports `{shard}` and other ClickHouse macros resolved via `system.macros`. There is NO `query_macros` or `get_macros` method on `ChClient`. This needs to be implemented.

## No SIGHUP Handler

The current codebase only handles `ctrl_c` (SIGINT) for graceful shutdown. There is no SIGHUP signal handler for config hot-reload. This needs to be implemented using `tokio::signal::unix::signal(SignalKind::hangup())`.

## AppState Missing Watch Fields

`AppState` in `src/server/state.rs` has no fields for watch mode state. Watch-related state (current watch state, reload flag, etc.) needs to be added.
