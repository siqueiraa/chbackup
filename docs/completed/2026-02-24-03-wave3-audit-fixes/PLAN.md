# Plan: Fix 5 Wave-3 Audit Findings

## Goal

Fix 5 correctness and robustness issues identified during wave-3 code audit: a logic-operator bug in Distributed DDL remap, fragile backup-type classification in watch resume, missing API body parameter for watch/start, a validation gap gated on `watch.enabled`, and missing CLI flags for server watch intervals.

## Architecture Overview

All changes modify existing modules with no new files or architectural components:
- **src/restore/remap.rs** -- Fix boolean operator in guard clause (W3-1)
- **src/watch/mod.rs** -- Add `classify_backup_type()` helper to replace fragile substring matching (W3-2)
- **src/server/routes.rs** -- Add `WatchStartRequest` type and optional body to `watch_start` handler (W3-3)
- **src/config.rs** -- Remove `watch.enabled` gate from interval validation (W3-4)
- **src/cli.rs** + **src/main.rs** -- Add `--watch-interval`/`--full-interval` flags to Server CLI variant and wire into config (W3-5)

## Architecture Assumptions (VALIDATED)

### Component Ownership
- **remap.rs**: Private function `rewrite_distributed_engine` called only by `rewrite_create_table_ddl()` (line 219). No external callers.
- **watch/mod.rs**: `resume_state()` is public, called by `run_watch_loop()` (line 351) and 7 test functions.
- **routes.rs**: `watch_start()` is a public handler registered in `server/mod.rs:83`. `spawn_watch_from_state()` is defined in `server/mod.rs:381`.
- **config.rs**: `Config::validate()` is public, called during config load and hot-reload paths.
- **cli.rs**: `Command::Server` variant destructured in `main.rs:652`.

### What This Plan CANNOT Do
- Cannot add integration tests (require real ClickHouse + S3)
- Cannot change `spawn_watch_from_state()` signature without updating both callers (watch_start API + start_server)
- W3-3 interval overrides must go through config mutation before `spawn_watch_from_state()`, not after

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| W3-1: Operator change affects correctness | GREEN | De Morgan's law is straightforward; regression test covers partial-match scenario |
| W3-2: Template parsing edge cases | YELLOW | Tested with default template, custom templates, and no `{type}` in template; fallback to FullNow on None |
| W3-3: Body parsing breaks existing callers | GREEN | `Option<Json<WatchStartRequest>>` makes body optional; no-body calls work exactly as before |
| W3-4: Removing validation gate breaks configs | GREEN | Default intervals ("1h" and "24h") always pass validation; only catches genuinely invalid configs |
| W3-5: CLI flag wiring | GREEN | Copy-paste from existing Watch variant pattern in cli.rs and main.rs |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Watch loop started via API` | yes (W3-3) | Confirms watch_start handler executed successfully |
| `ERROR:` | no (forbidden) | Should NOT appear during normal operation |

**Note:** W3-1, W3-2, W3-4, W3-5 are compile-time and unit-test-only changes with no runtime log markers needed. W3-3's existing log line at routes.rs:1624 serves as runtime evidence.

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| `spawn_watch_from_state` does not validate merged config | Config validation runs at load time; API-provided overrides could bypass it | Addressed in W3-3 by adding explicit validation before spawn |
| Watch resume does not handle templates without `{type}` gracefully | Existing behavior returns `FullNow` fallback; W3-2 makes this explicit via `classify_backup_type` returning None | N/A -- handled by fallback |

## Dependency Groups

```
Group A (Independent -- can run in parallel):
  - Task 1: W3-1 Fix Distributed remap condition
  - Task 2: W3-2 Add classify_backup_type helper
  - Task 3: W3-4 Remove watch.enabled validation gate

Group B (After Group A -- depends on config validation fix from T3):
  - Task 4: W3-3 watch/start accepts optional body
  - Task 5: W3-5 Server CLI watch flags

Group C (Always last -- after all code tasks):
  - Task 6: Update CLAUDE.md for watch/ and server/ modules
```

## Tasks

### Task 1: W3-1 Fix Distributed remap condition `&&` to `||`

**Finding:** In `rewrite_distributed_engine()` at remap.rs:647, the guard clause uses `&&` which only bails when NEITHER db NOR table matches. The correct logic is `||` to bail when EITHER doesn't match (rewrite only when BOTH match).

**TDD Steps:**

1. **Write failing test** `test_rewrite_ddl_distributed_partial_match_db`:
   - Input: DDL with `Distributed('cluster', other_db, src_table, rand())`
   - Call `rewrite_create_table_ddl()` with `src_db="src_db"`, `src_table="src_table"`
   - Assert: DDL returned UNCHANGED (table arg is `src_table` but db arg is `other_db`)
   - This test FAILS with `&&` because the guard lets through partial matches

2. **Write failing test** `test_rewrite_ddl_distributed_partial_match_table`:
   - Input: DDL with `Distributed('cluster', src_db, other_table, rand())`
   - Call `rewrite_create_table_ddl()` with `src_db="src_db"`, `src_table="src_table"`
   - Assert: DDL returned UNCHANGED (db arg is `src_db` but table arg is `other_table`)

3. **Fix**: Change line 647 from `&&` to `||`:
   ```rust
   if db_val != src_db || table_val != src_table {
       return ddl.to_string();
   }
   ```

4. **Verify** both new tests pass and existing tests (`test_rewrite_ddl_distributed_table`, `test_rewrite_ddl_distributed_quoted`) still pass.

**Files:** `src/restore/remap.rs`
**Acceptance:** F001

---

### Task 2: W3-2 Add `classify_backup_type` helper

**Finding:** Lines 145-146 of `watch/mod.rs` use `b.name.contains("full")` / `b.name.contains("incr")` which is fragile when shard names or other template segments contain these substrings.

**TDD Steps:**

1. **Write unit tests** for `classify_backup_type`:

   a. `test_classify_backup_type_default_template`:
      - Template: `"shard{shard}-{type}-{time:%Y%m%d_%H%M%S}"`
      - Name: `"shard01-full-20250315_120000"` -> returns `Some("full")`
      - Name: `"shard01-incr-20250315_130000"` -> returns `Some("incr")`

   b. `test_classify_backup_type_prefix_only`:
      - Template: `"{type}-backup-{time:%Y%m%d}"`
      - Name: `"full-backup-20250315"` -> returns `Some("full")`
      - Name: `"incr-backup-20250315"` -> returns `Some("incr")`

   c. `test_classify_backup_type_no_type_placeholder`:
      - Template: `"daily-{time:%Y%m%d}"` (no `{type}`)
      - Name: `"daily-20250315"` -> returns `None`

   d. `test_classify_backup_type_ambiguous_name`:
      - Template: `"shard{shard}-{type}-{time:%Y%m%d}"`
      - Name: `"shard01-fullmoon-20250315"` -> returns `None` (not "full" or "incr")

   e. `test_classify_backup_type_type_at_end`:
      - Template: `"backup-{time:%Y%m%d}-{type}"`
      - Name: `"backup-20250315-full"` -> returns `Some("full")`

2. **Implement** `classify_backup_type`:
   ```rust
   /// Classify a backup name as "full" or "incr" based on the template structure.
   ///
   /// Finds where `{type}` appears in the template, extracts the corresponding
   /// substring from `name`, and checks if it matches "full" or "incr".
   /// Returns `None` if:
   /// - Template has no `{type}` placeholder
   /// - Name doesn't match the template prefix before `{type}`
   /// - Extracted token is neither "full" nor "incr"
   pub fn classify_backup_type<'a>(template: &str, name: &'a str) -> Option<&'static str> {
       // Find {type} in template
       let type_marker = "{type}";
       let type_pos = template.find(type_marker)?;

       // Extract prefix before {type}
       let prefix = &template[..type_pos];

       // Verify name starts with the resolved prefix length
       // But prefix may contain other macros like {shard} -- we need the static prefix
       // Use resolve_template_prefix logic: everything before first {
       // However {type} might not be the first macro. We need the portion before {type}
       // that is purely static (no macros).

       // Strategy: find the last static character before {type} position,
       // and the first static character after {type}.
       // The static prefix is everything in template before the first '{' that precedes {type}.

       // Simpler approach: scan template for {type}, get chars before and after.
       // The prefix before {type} might contain macros. We can't match those directly.
       // Instead, use the separator character immediately before {type} (if any)
       // and after {type} to delimit the token in the name.

       // Find the segment of template after {type}
       let after_type = &template[type_pos + type_marker.len()..];
       // The delimiter after {type} is the first character of after_type,
       // or end-of-string if after_type is empty.
       let end_delim: Option<char> = after_type.chars().next().filter(|c| *c != '{');

       // The delimiter before {type} is the last character of prefix (if static)
       let start_delim: Option<char> = prefix.chars().last().filter(|c| *c != '}');

       // Extract token from name:
       // If we have a start_delim, find the last occurrence of it in name
       // and take the substring after it.
       // If we have an end_delim, find the first occurrence of it after start
       // and take the substring before it.

       let token_start = match start_delim {
           Some(d) => {
               // Find the position in name that corresponds to the delimiter before {type}
               // We need to find d in name. Since macros before {type} have variable length,
               // we look for the delimiter from the approximate position.
               // Use the last occurrence up to a reasonable search window.
               name.rfind(d).map(|p| p + d.len_utf8())?
           }
           None => 0, // {type} is at the start of template
       };

       let token_end = match end_delim {
           Some(d) => name[token_start..].find(d).map(|p| token_start + p)
               .unwrap_or(name.len()),
           None => name.len(), // {type} is at the end of template
       };

       let token = &name[token_start..token_end];
       match token {
           "full" => Some("full"),
           "incr" => Some("incr"),
           _ => None,
       }
   }
   ```

3. **Replace** lines 145-146 in `resume_state()`:
   ```rust
   // Before:
   // let last_full = matching.iter().find(|b| b.name.contains("full"));
   // let last_incr = matching.iter().find(|b| b.name.contains("incr"));

   // After:
   let last_full = matching.iter().find(|b| {
       classify_backup_type(name_template, &b.name) == Some("full")
   });
   let last_incr = matching.iter().find(|b| {
       classify_backup_type(name_template, &b.name) == Some("incr")
   });
   ```

4. **Verify** all existing `test_resume_*` tests pass (they use template `"shard1-{type}-{time:%Y%m%d}"` with names like `"shard1-full-20250315"` which will classify correctly).

**Files:** `src/watch/mod.rs`
**Acceptance:** F002

---

### Task 3: W3-4 Remove `watch.enabled` gate from interval validation

**Finding:** In `config.rs:1400`, interval validation is gated on `self.watch.enabled`, but watch can be started via CLI `--watch` flag, `chbackup watch` command, or API without setting `watch.enabled=true` in config.

**TDD Steps:**

1. **Write failing test** `test_validate_watch_intervals_always_checked`:
   - Create a Config with `watch.enabled = false`, `watch.watch_interval = "25h"`, `watch.full_interval = "24h"` (invalid: full < watch)
   - Call `config.validate()`
   - Assert: returns Err (currently passes because of the `enabled` gate)

2. **Fix**: Remove the `if self.watch.enabled {` guard and its closing `}` at lines 1400 and 1412:
   ```rust
   // Before:
   // if self.watch.enabled {
   //     let watch_secs = ...
   //     ...
   // }

   // After (always validate):
   let watch_secs = parse_duration_secs(&self.watch.watch_interval)
       .context("Invalid watch.watch_interval duration")?;
   let full_secs = parse_duration_secs(&self.watch.full_interval)
       .context("Invalid watch.full_interval duration")?;
   if full_secs <= watch_secs {
       return Err(anyhow::anyhow!(
           "watch.full_interval ({}) must be greater than watch.watch_interval ({})",
           self.watch.full_interval,
           self.watch.watch_interval
       ));
   }
   ```

3. **Verify** existing tests pass (default intervals "1h" and "24h" satisfy 86400 > 3600).

**Files:** `src/config.rs`
**Acceptance:** F003

---

### Task 4: W3-3 POST /api/v1/watch/start accepts optional body

**Finding:** `watch_start` handler has no body parameter. Users cannot override `watch_interval` or `full_interval` when starting watch via API.

**TDD Steps:**

1. **Define `WatchStartRequest`** struct (follows `CreateRequest` pattern at routes.rs:743):
   ```rust
   /// Optional request body for POST /api/v1/watch/start
   #[derive(Debug, Deserialize, Default)]
   pub struct WatchStartRequest {
       pub watch_interval: Option<String>,
       pub full_interval: Option<String>,
   }
   ```

2. **Modify `watch_start` signature** to accept optional body:
   ```rust
   pub async fn watch_start(
       State(mut state): State<AppState>,
       body: Option<Json<WatchStartRequest>>,
   ) -> Result<Json<WatchActionResponse>, (StatusCode, Json<ErrorResponse>)> {
       let req = body.map(|Json(r)| r).unwrap_or_default();
   ```

3. **Apply overrides and validate** before spawning:
   After the "Check if watch is already active" block and before `spawn_watch_from_state`, add:
   ```rust
   // Apply optional interval overrides
   if req.watch_interval.is_some() || req.full_interval.is_some() {
       let mut config = (*state.config.load_full()).clone();
       if let Some(v) = req.watch_interval {
           config.watch.watch_interval = v;
       }
       if let Some(v) = req.full_interval {
           config.watch.full_interval = v;
       }
       // Validate merged config before spawning
       config.validate().map_err(|e| {
           (
               StatusCode::BAD_REQUEST,
               Json(ErrorResponse {
                   error: format!("invalid config: {}", e),
               }),
           )
       })?;
       state.config.store(Arc::new(config));
   }
   ```

4. **Verify** no-body calls still work (unwrap_or_default gives WatchStartRequest with None fields, skip override block entirely).

**Implementation Notes:**
- `Config` must implement `Clone`. Verify with grep. If not, this approach needs adjustment.
- The `state.config.store()` call atomically swaps the config for all handlers via ArcSwap.
- Validation runs W3-4's fix (always validates intervals regardless of `watch.enabled`).

**Files:** `src/server/routes.rs`
**Acceptance:** F004

---

### Task 5: W3-5 Add `--watch-interval`/`--full-interval` to Server CLI variant

**Finding:** `Command::Server` only has `watch: bool` flag. Users must edit config to change intervals when using `chbackup server --watch`.

**TDD Steps:**

1. **Add flags to `Command::Server`** in cli.rs (copy from `Command::Watch` pattern):
   ```rust
   Server {
       /// Enable watch loop alongside API server
       #[arg(long)]
       watch: bool,

       /// Override watch interval (e.g. 1h, 30m)
       #[arg(long = "watch-interval")]
       watch_interval: Option<String>,

       /// Override full backup interval (e.g. 24h)
       #[arg(long = "full-interval")]
       full_interval: Option<String>,
   },
   ```

2. **Wire overrides in main.rs** (copy from Watch command pattern at main.rs:540-559):
   ```rust
   Command::Server { watch, watch_interval, full_interval } => {
       let mut config = config;
       if let Some(v) = watch_interval {
           config.watch.watch_interval = v;
       }
       if let Some(v) = full_interval {
           config.watch.full_interval = v;
       }
       let ch = ChClient::new(&config.clickhouse)?;
       let s3 = S3Client::new(&config.s3).await?;
       let config_path = PathBuf::from(&cli.config);
       chbackup::server::start_server(Arc::new(config), ch, s3, watch, config_path).await?;
   }
   ```

3. **Write test** `test_server_cli_watch_interval_flags`:
   - Parse `Cli::try_parse_from(["chbackup", "server", "--watch", "--watch-interval", "2h", "--full-interval", "48h"])`
   - Assert: `watch_interval == Some("2h")`, `full_interval == Some("48h")`, `watch == true`

4. **Verify** existing `Command::Server { watch }` destructuring compiles (must update the pattern to include new fields).

**Files:** `src/cli.rs`, `src/main.rs`
**Acceptance:** F005

---

### Task 6: Update CLAUDE.md for all modified modules (MANDATORY)

**Purpose:** Ensure all module documentation reflects code changes from this plan.

**Modules to update:** src/watch, src/server (from affected-modules.json)

**TDD Steps:**

1. **Update `src/watch/CLAUDE.md`:**
   - Add `classify_backup_type(template, name) -> Option<&'static str>` to Public API section
   - Update Resume State section to mention template-aware classification instead of substring matching
   - Regenerate directory tree if needed (single-file module, unlikely to change)

2. **Update `src/server/CLAUDE.md`:**
   - Update Watch API endpoints section to document `WatchStartRequest` optional body parameter with `watch_interval` and `full_interval` fields
   - Add `WatchStartRequest` to the request types list

3. **Validate required sections exist:**
   - Each CLAUDE.md has: Parent Context, Directory Structure, Key Patterns, Parent Rules

**Files:** `src/watch/CLAUDE.md`, `src/server/CLAUDE.md`
**Acceptance:** FDOC

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 | PASS | All APIs verified via LSP hover and direct file reads (see context/symbols.md) |
| RC-008 | PASS | T4 uses Config::validate() which T3 fixes -- T3 is in Group A (before T4 in Group B) |
| RC-015 | PASS | No cross-task data flow; each task is self-contained |
| RC-016 | PASS | WatchStartRequest fields match usage in T4 implementation |
| RC-017 | PASS | All F-IDs (F001-F005, FDOC) match acceptance.json |
| RC-018 | PASS | Task dependency ordering validated: A before B before C |
| RC-019 | PASS | WatchStartRequest follows CreateRequest pattern; Server CLI follows Watch CLI pattern |
| RC-021 | PASS | All file locations verified via grep (see context/references.md) |
| RC-035 | PASS | cargo fmt required after all changes (per zero-warnings policy) |

## Cross-Task Type Consistency

- `WatchStartRequest` defined in T4, used only in T4 (no cross-task dependency)
- `classify_backup_type` defined in T2, used only in T2's modification to `resume_state` (no cross-task dependency)
- `Config::validate()` modified in T3, called from T4's watch_start handler (T3 precedes T4)
- `Command::Server` modified in T5, destructured in T5's main.rs changes (same task)

## Notes

### Phase 4.5 Skip Justification
Interface skeleton simulation is skipped because:
- All changes are within existing functions or add new items to existing modules
- No new cross-module imports are introduced
- All type signatures verified via LSP hover (documented in context/symbols.md)
- The plan adds one new public function (`classify_backup_type`) and one new struct (`WatchStartRequest`), both confined to their respective modules

### Config Clone Verification
Task 4 requires `Config` to implement `Clone` for the `(*state.config.load_full()).clone()` call. This must be verified during execution. If `Config` does not derive `Clone`, an alternative approach is to read config fields individually rather than cloning the entire struct.
