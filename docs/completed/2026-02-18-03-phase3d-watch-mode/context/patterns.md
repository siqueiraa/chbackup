# Pattern Discovery

## Global Patterns Registry

No global patterns directory found at `docs/patterns/`. Kameo actor patterns are not applicable (chbackup uses plain async Rust with tokio).

## Component Identification

### Components for Watch Mode

| Component | Type | Status |
|-----------|------|--------|
| Watch state machine | New module (`src/watch/`) | TO CREATE |
| Name template resolver | New function | TO CREATE |
| SIGHUP handler | New async signal handler | TO CREATE |
| Watch API endpoints | Existing stubs in routes.rs | TO REPLACE |
| Watch metrics | Existing fields in Metrics struct | TO WIRE |
| Config hot-reload | New function | TO CREATE |

### Existing Components to Reuse

| Component | Location | How Used |
|-----------|----------|----------|
| `backup::create()` | src/backup/mod.rs:64 | Called in watch Create step |
| `upload::upload()` | src/upload/mod.rs:165 | Called in watch Upload step |
| `list::list_remote()` | src/list.rs:128 | Called in watch Resume step |
| `list::retention_local()` | src/list.rs:411 | Called in watch Retention step |
| `list::retention_remote()` | src/list.rs:629 | Called in watch Retention step |
| `list::delete_local()` | src/list.rs:220 | Called in watch Delete-local step |
| `list::effective_retention_local()` | src/list.rs:380 | Config resolution |
| `list::effective_retention_remote()` | src/list.rs:393 | Config resolution |
| `WatchConfig` | src/config.rs:392 | Watch configuration |
| `Config::load()` | src/config.rs:810 | Config hot-reload |
| `AppState` | src/server/state.rs:26 | Server-watch integration |
| `Metrics` | src/server/metrics.rs:18 | Watch metric fields already registered |
| `BackupManifest` | src/manifest.rs:19 | Name extraction from remote backups |
| `BackupSummary` | src/list.rs:28 | Remote backup listing |

## Pattern Analysis: Operation Lifecycle in Server

Reference: `routes.rs` create_remote handler (lines 613-717)

Pattern for chaining create + upload:
1. Call `backup::create()` with config params
2. If create fails, mark operation as failed, return
3. Call `upload::upload()` with backup_dir derived from config.clickhouse.data_path
4. If upload fails, mark operation as failed
5. On success, update metrics

The watch loop will follow this exact pattern but within a loop with additional:
- Incremental diff-from logic (pass previous backup name)
- Delete-local step (via `list::delete_local()`)
- Retention step (via `list::retention_local()` + `list::retention_remote()`)
- Error tracking and full-backup fallback

## Pattern Analysis: Duration Parsing

`parse_duration_secs()` in config.rs (line 1248) is private. It supports `h`, `m`, `s` suffixes.
Watch module needs to parse `watch_interval`, `full_interval`, `retry_interval` strings into `std::time::Duration`.
Options:
1. Make `parse_duration_secs` public
2. Duplicate in watch module
3. Create a shared utility

Decision: Make `parse_duration_secs` public in config.rs (simplest, avoids duplication).

## Pattern Analysis: SIGHUP Handling

No existing SIGHUP handler in the codebase. The server uses `tokio::signal::ctrl_c()` for shutdown (src/server/mod.rs:183).

For SIGHUP, use `tokio::signal::unix::signal(SignalKind::hangup())` which returns a `Signal` stream. This is the standard tokio pattern.

## Pattern Analysis: Config Hot-Reload

No existing config reload in the codebase. Config is loaded once in main.rs (line 83) and passed as `Arc<Config>`.

For hot-reload, need:
1. Re-read config file from same path
2. Validate new config
3. Replace the `Arc<Config>` in `AppState` -- but `AppState.config` is `Arc<Config>` (immutable)
4. Need to change to `Arc<tokio::sync::RwLock<Arc<Config>>>` or use `arc_swap::ArcSwap<Config>`

Simpler approach: watch loop holds its own config reference and reloads into its local state. Only watch-relevant fields (intervals, name_template, retention counts) need refreshing. Server routes continue using original config for operations.

## Pattern Analysis: Background Task in Server

Server uses `tokio::spawn` extensively for background operations (routes.rs). Watch loop will be spawned as a long-lived background task from `start_server()` when `--watch` flag is set or `watch.enabled` config is true.

Key pattern from auto_resume (state.rs:219-372):
```rust
tokio::spawn(async move {
    // long-running task with access to state_clone
});
```

## WatchConfig Fields (Already Defined)

```rust
pub struct WatchConfig {
    pub enabled: bool,                    // enable watch loop in server mode
    pub watch_interval: String,           // "1h"
    pub full_interval: String,            // "24h"
    pub name_template: String,            // "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}"
    pub max_consecutive_errors: u32,      // 5
    pub retry_interval: String,           // "5m"
    pub delete_local_after_upload: bool,  // true
}
```

**Missing from WatchConfig vs design 10.3:** The design mentions `tables: "*.*"` as a watch-level config but this is NOT in WatchConfig. Currently table filtering comes from `backup.tables` or CLI `-t` flag. The watch CLI command already accepts `-t/--tables`. This is fine -- table filter can be passed from CLI or config.backup.tables.
