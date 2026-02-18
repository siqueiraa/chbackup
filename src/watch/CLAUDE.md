# CLAUDE.md -- src/watch

## Parent Context

Parent: [/CLAUDE.md](../../CLAUDE.md)

This module implements the watch mode scheduler (design doc section 10) that maintains a rolling chain of full + incremental backups. It runs as a long-lived process, either standalone (`chbackup watch`) or as a background task in server mode (`chbackup server --watch`).

## Directory Structure

```
src/watch/
  mod.rs        -- Watch state machine, name template resolution, resume logic, all types
```

## Key Patterns

### Name Template Resolution (resolve_name_template)
Resolves backup name templates with macro substitution. Supported placeholders:
- `{type}` -- replaced with `backup_type` ("full" or "incr")
- `{time:FORMAT}` -- replaced with `now.format(FORMAT)` using chrono strftime syntax
- `{macro_name}` -- replaced from a HashMap of ClickHouse `system.macros` values (e.g., `{shard}` -> "01")

Unrecognized `{...}` patterns are left as-is (not removed). Uses a char-by-char parser, not regex.

### Resume State (resume_state)
Determines what the watch loop should do next by examining remote backups:
1. Filters backups by template prefix (`resolve_template_prefix`) and excludes broken ones
2. Finds most recent full and incremental backups by name substring ("full" / "incr")
3. Compares elapsed time against `watch_interval` and `full_interval`
4. Returns a `ResumeDecision`:
   - `FullNow` -- no matching backups, or full interval has elapsed
   - `IncrNow { diff_from }` -- incremental is overdue; `diff_from` is the most recent backup name
   - `SleepThen { remaining, backup_type }` -- most recent backup is still fresh, sleep first

Template prefix filtering (design 10.5) prevents picking up unrelated backups: e.g., template `"shard1-{type}-{time}"` only considers backups starting with `"shard1-"`.

### Watch State Machine (run_watch_loop)
The main loop implements design 10.4 as a state machine:
1. **Resume**: Query remote backups via `list_remote()`, call `resume_state()`
2. **Decide**: Check `force_next_full` flag or `full_interval` elapsed -> full; otherwise incremental
3. **Create**: Call `backup::create()` with resolved name template
4. **Upload**: Call `upload::upload()`
5. **Delete local**: If `delete_local_after_upload` configured, call `list::delete_local()` via `spawn_blocking`
6. **Retention**: Best-effort (design 10.7) -- `retention_local()` via `spawn_blocking`, `retention_remote()` async
7. **Sleep**: Interruptible via `tokio::select!` for shutdown/reload signals
8. **Error**: Increment `consecutive_errors`, set `force_next_full=true`, sleep `retry_interval`

State transitions update `WatchState` enum and corresponding Prometheus metrics.

### WatchState Metric Encoding
`WatchState` maps to `chbackup_watch_state` IntGauge:
- 1 = Idle, 2 = CreatingFull, 3 = CreatingIncr, 4 = Uploading, 5 = Cleaning, 6 = Sleeping, 7 = Error
- 0 = inactive (watch not running, set externally)

### Config Hot-Reload (apply_config_reload)
Triggered by SIGHUP (Unix) or `/api/v1/reload` endpoint. Behavior per design 10.8:
- Current cycle completes first (no mid-operation interruption)
- Reload happens at next sleep entry via `reload_rx` channel
- Reads config from file via `Config::load()`, validates via `config.validate()`
- On validation failure: logs warning, retains current config
- On success: logs old->new values for key watch parameters, replaces `ctx.config`

### Error Recovery (RC-011 Compliance)
Every state flag has explicit set/clear paths:
- `force_next_full`: set on error (`handle_error`), cleared after successful full backup
- `consecutive_errors`: incremented on error, reset to 0 on any successful cycle
- `reload` channel: drained during `interruptible_sleep`, not persisted

### Interruptible Sleep
`interruptible_sleep()` uses `tokio::select!` over three futures:
- `tokio::time::sleep(duration)` -- normal expiry
- `shutdown_rx.changed()` -- returns `WatchLoopExit::Shutdown`
- `reload_rx.changed()` -- triggers `apply_config_reload()`, continues sleeping

### Sync Functions in Async Context
`list::retention_local()` and `list::delete_local()` are sync functions. They are called via `tokio::task::spawn_blocking` to avoid blocking the async runtime, matching the pattern used in `server/routes.rs`.

## Public API

- `resolve_name_template(template, backup_type, now, macros) -> String` -- Substitute template placeholders
- `resolve_template_prefix(name_template) -> String` -- Extract static prefix before first `{`
- `resume_state(backups, name_template, watch_interval, full_interval, now) -> ResumeDecision` -- Determine next action
- `run_watch_loop(ctx: WatchContext) -> WatchLoopExit` -- Main state machine loop (async)

## Types

- `ResumeDecision` -- Enum: `FullNow`, `IncrNow { diff_from }`, `SleepThen { remaining, backup_type }`
- `WatchState` -- Enum with metric values: Idle(1), CreatingFull(2), CreatingIncr(3), Uploading(4), Cleaning(5), Sleeping(6), Error(7)
- `WatchLoopExit` -- Enum: `Shutdown`, `MaxErrors`, `Stopped`
- `WatchContext` -- Struct holding all loop state: config, clients, metrics, channels, macros, error tracking

## Error Handling

- Backup create/upload failures: logged as warnings, trigger `handle_error()` (increment errors, set force_next_full, sleep retry_interval)
- Retention failures: logged as warnings, best-effort (continue cycle)
- Config reload failures: logged as warnings, retain current config
- Remote listing failures: treated as errors (triggers error recovery)
- `max_consecutive_errors` (0 = unlimited): exits loop with `WatchLoopExit::MaxErrors` when threshold reached

## Parent Rules

All rules from [/CLAUDE.md](../../CLAUDE.md) apply:
- Zero warnings policy
- Conventional commits
- Integration tests require real ClickHouse + S3
