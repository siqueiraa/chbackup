# Symbol and Reference Analysis

## W3-1: Distributed remap condition `&&` -> `||`

### Symbol: `rewrite_distributed_engine`
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/restore/remap.rs`
- **Line:** 599 (function definition)
- **Visibility:** `fn` (private, module-internal)
- **Caller:** `rewrite_create_table_ddl()` at line 219 (the only call site)

### Bug Location: Line 647
```rust
// Only rewrite if the source matches
if db_val != src_db && table_val != src_table {
    return ddl.to_string();
}
```

**Analysis:** This is a guard clause that should bail out (return DDL unchanged) when the Distributed engine arguments do NOT match the source. The current `&&` means "bail if BOTH don't match" -- so if only ONE matches (e.g., db matches but table doesn't), the function proceeds to rewrite, which is incorrect. The correct logic is `||` -- "bail if EITHER doesn't match" -- meaning rewrite only when BOTH db AND table match the source.

**De Morgan's law confirmation:**
- Current: `!(db == src_db) && !(table == src_table)` = "bail when neither matches"
- Fixed: `!(db == src_db) || !(table == src_table)` = "bail when any one doesn't match"
- Equivalent to: `!(db == src_db && table == src_table)` = "bail unless both match"

### Test Coverage
- `test_rewrite_ddl_distributed_table` (line 1092): Tests the happy path where BOTH db and table match -- passes with `&&` because both comparisons are equal (neither `!=` is true), so the guard doesn't trigger
- `test_rewrite_ddl_distributed_quoted` (line 1117): Same -- both match
- **Missing test:** No test for partial match (db matches, table doesn't; or vice versa) -- the bug is undetectable by existing tests

### Upstream Callers of `rewrite_create_table_ddl`
- `rewrite_create_table_ddl()` is a public function called from:
  - `src/restore/schema.rs` (in `create_tables()` and `create_ddl_objects()`)
  - Tests in `src/restore/remap.rs`

---

## W3-2: Watch resume type classification

### Symbol: `resume_state`
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/watch/mod.rs`
- **Line:** 120 (function definition)
- **Visibility:** `pub fn`
- **References:** 8 call sites (line 351 in `run_watch_loop`, and 7 test functions)

### Bug Location: Lines 145-146
```rust
let last_full = matching.iter().find(|b| b.name.contains("full"));
let last_incr = matching.iter().find(|b| b.name.contains("incr"));
```

**Analysis:** Substring matching on backup names is fragile. If the name template does not include "full"/"incr" substrings, or if a shard name happens to contain "full" (e.g., "fullfilling-shard"), the classification breaks. The template uses `{type}` which resolves to "full" or "incr", but the position within the name matters for correctness.

### Template Resolution Context
- `resolve_name_template()` (line 34): `{type}` is replaced with `backup_type` ("full" or "incr")
- `resolve_template_prefix()` (line 107): Extracts prefix before first `{`
- Default template: `default_name_template()` in config.rs

### Related Functions
- `run_watch_loop()` (line 273): Calls `resume_state()` at line 351 with template from config
- The backup_type ("full"/"incr") is determined by the caller, not by `resume_state()`

### Proposed Helper: `classify_backup_type(template, name)`
Must parse the template structure to determine where `{type}` appears, extract the corresponding substring from the backup name, and classify as "full"/"incr"/unknown.

---

## W3-3: POST /api/v1/watch/start body parameter

### Symbol: `watch_start`
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/server/routes.rs`
- **Line:** 1601 (function definition)
- **Visibility:** `pub async fn`
- **References:** 2 (route registration at `src/server/mod.rs:83`, definition at line 1601)

### Current Signature
```rust
pub async fn watch_start(
    State(mut state): State<AppState>,
) -> Result<Json<WatchActionResponse>, (StatusCode, Json<ErrorResponse>)>
```

**Analysis:** No body parameter. Users cannot override `watch_interval` or `full_interval` via the API. The existing pattern for optional bodies is used extensively in this file (e.g., `create_backup` at line 683 uses `body: Option<Json<CreateRequest>>`).

### Existing Pattern
```rust
// From create_backup (line 683-686):
pub async fn create_backup(
    State(state): State<AppState>,
    body: Option<Json<CreateRequest>>,
) -> Result<Json<OperationStarted>, (StatusCode, Json<ErrorResponse>)> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
```

### Struct needed: `WatchStartRequest`
Fields: `watch_interval: Option<String>`, `full_interval: Option<String>`
Derives: `Debug, Deserialize, Default`

### Integration with `spawn_watch_from_state`
- At line 1622: `super::spawn_watch_from_state(&mut state, config_path, macros).await;`
- `spawn_watch_from_state()` reads config via `state.config.load_full()` (line 400 of mod.rs)
- To apply overrides: modify the loaded config clone before passing to WatchContext

---

## W3-4: watch.enabled gate on interval validation

### Symbol: `Config::validate`
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/config.rs`
- **Line:** 1380 (function definition)

### Bug Location: Lines 1398-1412
```rust
if self.watch.enabled {
    let watch_secs = parse_duration_secs(&self.watch.watch_interval)
        .context("Invalid watch.watch_interval duration")?;
    let full_secs = parse_duration_secs(&self.watch.full_interval)
        .context("Invalid watch.full_interval duration")?;
    if full_secs <= watch_secs {
        return Err(anyhow::anyhow!(...));
    }
}
```

**Analysis:** The `watch.enabled` guard means interval validation is skipped when:
1. `server --watch` flag is used (CLI flag, not config `watch.enabled`)
2. `POST /api/v1/watch/start` API endpoint is called
3. `chbackup watch` CLI command is used

In all three cases, the watch loop runs with potentially invalid intervals (e.g., `full_interval <= watch_interval`) because validation was gated on the config-level `watch.enabled` flag.

### How watch is enabled
- Config: `watch.enabled: true` in YAML -> validation runs
- CLI `chbackup watch`: Does NOT set `config.watch.enabled` -> validation SKIPPED
- CLI `chbackup server --watch`: Does NOT set `config.watch.enabled` -> validation SKIPPED
- API `/api/v1/watch/start`: Does NOT set `config.watch.enabled` -> validation SKIPPED
- `start_server()` line 152: `let watch_enabled = watch || config.watch.enabled;`

### Fix
Remove the `if self.watch.enabled` guard so interval validation always runs. The intervals have defaults ("1h" and "24h") that always pass validation, so this won't break configs that don't use watch.

### Related Function: `parse_duration_secs`
- **Line:** 1542
- **Visibility:** `pub fn`
- Parses "1h", "30m", "24h" etc. to seconds

---

## W3-5: --watch-interval/--full-interval flags on Server CLI variant

### Symbol: `Command::Server`
- **File:** `/Users/rafael.siqueira/dev/personal/chbackup/src/cli.rs`
- **Line:** 344-348

### Current Definition
```rust
Server {
    /// Enable watch loop alongside API server
    #[arg(long)]
    watch: bool,
},
```

### Existing Pattern in `Command::Watch` (line 325-341)
```rust
Watch {
    #[arg(long = "watch-interval")]
    watch_interval: Option<String>,
    #[arg(long = "full-interval")]
    full_interval: Option<String>,
    #[arg(long = "name-template")]
    name_template: Option<String>,
    #[arg(short = 't', long = "tables")]
    tables: Option<String>,
},
```

### Server command dispatch in main.rs (line 652-657)
```rust
Command::Server { watch } => {
    let ch = ChClient::new(&config.clickhouse)?;
    let s3 = S3Client::new(&config.s3).await?;
    let config_path = PathBuf::from(&cli.config);
    chbackup::server::start_server(Arc::new(config), ch, s3, watch, config_path).await?;
}
```

### How Watch CLI applies overrides in main.rs (lines 540-559)
```rust
Command::Watch { watch_interval, full_interval, name_template, tables } => {
    let mut config = config;
    if let Some(v) = watch_interval { config.watch.watch_interval = v; }
    if let Some(v) = full_interval { config.watch.full_interval = v; }
    ...
}
```

### Fix
1. Add `watch_interval: Option<String>` and `full_interval: Option<String>` to `Command::Server`
2. In main.rs, apply overrides to config before calling `start_server()` (same pattern as Watch command)

---

## Shared Dependencies

| Symbol | File | Used By |
|--------|------|---------|
| `parse_duration_secs` | `src/config.rs:1542` | W3-4 (config.validate), W3-2 (watch/mod.rs) |
| `WatchConfig` | `src/config.rs:405` | W3-4, W3-5 |
| `resolve_name_template` | `src/watch/mod.rs:34` | W3-2 (for understanding template structure) |
| `resolve_template_prefix` | `src/watch/mod.rs:107` | W3-2 (existing pattern for prefix extraction) |
| `AppState` | `src/server/state.rs` | W3-3 |
| `spawn_watch_from_state` | `src/server/mod.rs:381` | W3-3 |
