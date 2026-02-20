# Redundancy Analysis

## Proposed New Components

### 1. `watch` module (`src/watch/mod.rs` or `src/watch.rs`)

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `watch::run_watch_loop()` | None | N/A -- new functionality | - | No watch loop exists anywhere |
| `watch::resume_state()` | None | N/A -- new functionality | - | No resume-from-remote scan exists |
| `watch::resolve_name_template()` | None | N/A -- new functionality | - | No name template resolution exists |
| `watch::WatchState` (enum) | None | N/A -- new type | - | No watch state machine exists |

### 2. `ChClient::get_macros()`

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `ChClient::get_macros()` | None found in src/clickhouse/ | N/A -- new method | - | Grep for "macros" in clickhouse/ returned no results |

### 3. `parse_duration_secs` (make public)

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `pub fn parse_duration_secs()` | `fn parse_duration_secs()` in config.rs:1248 | EXTEND | - | Make existing private function public. No duplication. |

### 4. Watch API endpoint replacements

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `routes::watch_start()` | `routes::watch_start_stub()` at routes.rs:1110 | REPLACE | - | Replace 501 stub with real implementation |
| `routes::watch_stop()` | `routes::watch_stop_stub()` at routes.rs:1115 | REPLACE | - | Replace 501 stub with real implementation |
| `routes::watch_status()` | `routes::watch_status_stub()` at routes.rs:1120 | REPLACE | - | Replace 501 stub with real implementation |
| `routes::reload()` | `routes::reload_stub()` at routes.rs:1096 | REPLACE | - | Replace 501 stub with real implementation |

Note: These are in-place replacements of stubs, not new routes. The route registration in `build_router()` already exists.

### 5. Config hot-reload function

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| Config reload logic | `Config::load()` at config.rs:810 | REUSE | - | Call existing Config::load() with same path. No new function needed -- just call `Config::load(path, &[])` |

## No COEXIST or REPLACE-with-removal Decisions

All proposed components are either genuinely new (no existing equivalent) or extend/replace stubs.
No cleanup deadlines needed.
