# Affected Modules Analysis

## Summary

- **Modules to create:** 1 (src/watch)
- **Modules to update:** 2 (src/server, src/clickhouse)
- **Individual files to modify:** 4 (config.rs, main.rs, cli.rs, lib.rs)
- **Total affected:** 7

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action |
|--------|------------------|----------|--------|
| src/server | EXISTS | new_patterns, tree_change | UPDATE |
| src/clickhouse | EXISTS | new_patterns | UPDATE |
| src/watch | NEW | - | CREATE |

## Individual Files

| File | Change Description |
|------|-------------------|
| src/config.rs | Make `parse_duration_secs` public |
| src/main.rs | Wire standalone `watch` command to watch loop, pass `--watch` to server |
| src/cli.rs | No changes expected (Watch and Server subcommands already defined) |
| src/lib.rs | Add `pub mod watch;` declaration |

## CLAUDE.md Tasks

1. **Create:** `src/watch/CLAUDE.md` (new module -- watch state machine, name template, resume)
2. **Update:** `src/server/CLAUDE.md` (watch integration, replaced stubs, SIGHUP, reload API)
3. **Update:** `src/clickhouse/CLAUDE.md` (add get_macros method documentation)

## Detailed Changes Per Module

### src/watch (NEW)
- `mod.rs` -- Watch state machine: run_watch_loop(), resume_state(), WatchState enum, WatchContext struct
- Name template resolution: resolve_name_template() with {type}, {time:FORMAT}, {shard} macros
- Integration with backup::create, upload::upload, list retention functions
- SIGHUP signal handling (or receive flag from server)

### src/server (UPDATE)
- `mod.rs` -- Spawn watch loop task when `--watch` flag or `watch.enabled` config
- `routes.rs` -- Replace stub endpoints: watch_start, watch_stop, watch_status, reload
- `state.rs` -- Add watch handle/channel to AppState for start/stop/status queries
- `metrics.rs` -- No changes (metrics already registered, just need to be updated by watch loop)

### src/clickhouse (UPDATE)
- `client.rs` -- Add `get_macros() -> Result<HashMap<String, String>>` method

### src/config.rs (MODIFY)
- Change `fn parse_duration_secs` to `pub fn parse_duration_secs`

### src/main.rs (MODIFY)
- Wire `Command::Watch { .. }` to call `watch::run_watch_loop()` directly (standalone mode)
- For `Command::Server { watch: true }`, pass flag through to `start_server()`

### src/lib.rs (MODIFY)
- Add `pub mod watch;` module declaration
