# Watch Mode: Go vs Rust Parity Analysis

## Source Files Compared

**Go sources:**
- `pkg/backup/watch.go` -- `Watch()`, `calculatePrevBackupNameAndType()`, `NewBackupWatchName()`, `ValidateWatchParams()`
- `pkg/server/server.go` -- `RunWatch()`, `httpWatchHandler()`, `actionsWatchHandler()`, `handleWatchResponse()`
- `pkg/server/metrics/metrics.go` -- `APIMetrics` struct, `ExecuteWithMetrics()`
- `pkg/config/config.go` -- `GeneralConfig` watch fields

**Rust sources:**
- `src/watch/mod.rs` -- `run_watch_loop()`, `resume_state()`, `resolve_name_template()`, `interruptible_sleep()`, `apply_config_reload()`
- `src/server/mod.rs` -- `start_server()`, `spawn_watch_from_state()`
- `src/server/routes.rs` -- `watch_start()`, `watch_stop()`, `watch_status()`, `reload()`
- `src/server/metrics.rs` -- `Metrics` struct, watch gauges
- `src/config.rs` -- `WatchConfig` struct

---

## 1. Watch Loop State Machine

### Go
The Go implementation is a simple `for` loop with a `select` on `ctx.Done()` at the top:
```go
for {
    select {
    case <-ctx.Done():
        return ctx.Err()
    default:
        // reload config, create_remote, delete local, metrics, sleep
    }
}
```
There is no explicit state machine enum. The loop is linear: create_remote -> delete local -> update metrics -> update prevBackup state -> sleep -> check full_interval -> loop.

### Rust
The Rust implementation uses an explicit `WatchState` enum (Idle=1, CreatingFull=2, CreatingIncr=3, Uploading=4, Cleaning=5, Sleeping=6, Error=7) with metric updates at each transition. The loop is: resume_state -> decide -> create -> upload -> delete_local -> retention -> reset errors -> sleep.

### Gap: None (behavioral)
The Rust state machine is a superset of Go's. The explicit states and metric encoding are Rust additions that Go lacks. No functional gap.

---

## 2. Full vs Incremental Scheduling

### Go
- Uses **time-based** scheduling: tracks `lastBackup` and `lastFullBackup` timestamps.
- After a successful cycle, checks if `fullDuration - elapsed <= 0` to decide next type.
- Uses `"full"` and `"increment"` as type strings (NOT `"incr"`).
- After a successful full backup, sets `backupType = "increment"` for subsequent cycles.
- On error: `createRemoteErr != nil` means prevBackupName/prevBackupType are NOT updated, so next cycle retries. But it does NOT force next to be full -- it retries whatever type it was.
- **No explicit force_next_full flag** on error. The Go code just doesn't update prevBackupName, so the next attempt uses the same type.

### Rust
- Uses **resume_state()** function that examines remote backups at each loop iteration.
- Uses `"full"` and `"incr"` as type strings.
- On error: sets `force_next_full = true`, forcing the next backup to be full regardless.
- Resume logic filters by template prefix and examines backup names for "full"/"incr" substrings.

### Gap: BACKUP TYPE STRING MISMATCH
**Severity: Medium**

Go uses `"increment"` as the backup type, Rust uses `"incr"`. This means:
1. The `{type}` placeholder in name templates resolves differently: Go produces `shard01-increment-20250315` while Rust produces `shard01-incr-20250315`.
2. The `resume_state()` function in Rust searches for `"incr"` substring in backup names -- this would NOT match Go-created backups containing `"increment"`.
3. If a user migrates from Go to Rust (or runs them side by side), backup chains would be broken.

**Go code (watch.go line ~145):**
```go
if prevBackupType == "full" {
    backupType = "increment"
}
```

**Rust code (watch/mod.rs line ~384):**
```rust
ResumeDecision::IncrNow { diff_from } => {
    ("incr".to_string(), Some(diff_from.clone()))
}
```

### Gap: FORCE-FULL-ON-ERROR BEHAVIOR DIFFERENCE
**Severity: Low**

Go does NOT force a full backup after error. It simply retries the same backup type (since prevBackupName is not updated on failure, the next call to NewBackupWatchName uses the same backupType). Only when the full interval elapses does it switch to full.

Rust explicitly sets `force_next_full = true` on every error, which means after any transient error (e.g., network blip), Rust will create a full backup instead of an incremental. This is arguably more conservative but differs from Go.

### Gap: RESUME APPROACH DIFFERENCE
**Severity: Low (architectural, not bug)**

Go's `calculatePrevBackupNameAndType()` is called once at startup. It scans remote backups, finds the latest matching one, calculates time remaining, and optionally sleeps. After that, state is tracked in-memory only (`prevBackupName`, `lastBackup`, `lastFullBackup`).

Rust calls `resume_state()` at the **beginning of every loop iteration**, querying remote backups each time. This is more resilient to crashes (always has latest state) but introduces more S3 LIST calls per cycle.

---

## 3. Name Template Resolution

### Go
- `NewBackupWatchName()` calls `b.ch.ApplyMacros(ctx, template)` first, which resolves ClickHouse macros by querying `system.macros` on every call.
- Then replaces `{type}` with backupType.
- Then uses regex `{time:([^}]+)}` to find time patterns and replaces with `time.Now().Format(layout)`.
- **Validation**: Returns error if template does NOT contain `{time:layout}` -- backup names must have time for uniqueness.
- Uses **Go time format** (e.g., `20060102150405`).

### Rust
- `resolve_name_template()` is a pure function taking a macros HashMap (queried once at startup).
- Resolves `{type}`, `{time:FORMAT}`, and macros from the HashMap.
- Uses **chrono strftime format** (e.g., `%Y%m%d_%H%M%S`).
- No validation that `{time:...}` is present.

### Gap: TIME FORMAT SYNTAX DIFFERENCE
**Severity: Medium**

Go uses Go-style time format strings (`20060102150405` -- the magic reference date) while Rust uses strftime format (`%Y%m%d_%H%M%S`). This means:
- The default template in Go is `shard{shard}-{type}-{time:20060102150405}`
- The default template in Rust is `shard{shard}-{type}-{time:%Y%m%d_%H%M%S}`

Config files are NOT portable between Go and Rust for this field. However, since this is a Rust replacement (not a drop-in config-compatible tool), this may be acceptable. Users migrating must update their config.

### Gap: MACROS QUERIED ONCE vs EVERY CALL
**Severity: Low**

Go queries `system.macros` on every `NewBackupWatchName()` call (each loop iteration). Rust queries macros once at watch loop startup and caches them in `WatchContext.macros`. If macros change in ClickHouse during watch operation, Go picks it up immediately while Rust does not (until config reload recreates the WatchContext).

### Gap: NO TEMPLATE UNIQUENESS VALIDATION
**Severity: Low**

Go validates that the template contains `{time:layout}` and returns an error if missing (names must be unique). Rust does not perform this validation. A template like `{type}-backup` would silently produce duplicate names.

---

## 4. Resume on Restart

### Go
- `calculatePrevBackupNameAndType()` runs once at startup.
- Scans all remote backups with `GetRemoteBackups(ctx, true)`.
- Builds a regex from the template (replacing `{type}` and `{time:...}` with `\S+`).
- Iterates ALL remote backups (ascending order -- oldest first) to find the latest matching one.
- If a match is found, calculates time remaining and sleeps if needed.
- Determines whether next should be full or increment based on elapsed time.

### Rust
- `resume_state()` runs at each loop iteration (not just startup).
- Filters by template prefix (string prefix matching, not regex).
- Sorts by timestamp descending and searches for "full"/"incr" substrings.
- Returns a `ResumeDecision` enum with three variants.

### Gap: TEMPLATE MATCHING APPROACH
**Severity: Low**

Go uses regex matching (`\S+` replacing placeholders) which is more precise. Rust uses prefix matching (`resolve_template_prefix()` extracts everything before the first `{`). For most templates this produces the same results, but edge cases could differ:
- Template `backup-{type}-{time}` with prefix `"backup-"` would match `backup-unrelated-name` in Rust but not in Go (Go would only match if the rest looks like `\S+`-`\S+`).

### Gap: RESUME INITIAL SLEEP
**Severity: Low**

Go sleeps at startup in `calculatePrevBackupNameAndType()` if the last backup is still fresh. Rust handles this via `ResumeDecision::SleepThen` which triggers the same behavior through the main loop's sleep path. Functionally equivalent.

---

## 5. Config Hot-Reload (SIGHUP)

### Go
- Config reload happens **inside** the main watch loop on every iteration: `config.LoadConfig(config.GetConfigPath(cliCtx))`.
- If the CLI context is non-nil, config is reloaded from disk before each backup cycle.
- `ValidateWatchParams()` is re-called after reload.
- Reload failure logs a warning but continues with current config.
- **There is no SIGHUP handling for watch.** Config is just re-read every cycle.

### Rust
- Config reload is triggered by SIGHUP signal or `/api/v1/reload` endpoint.
- Reload happens during `interruptible_sleep()` -- mid-cycle operations are not interrupted.
- `Config::load()` + `validate()` is called, and on success the new config replaces the old.
- Key parameter changes are logged.

### Gap: RELOAD TIMING
**Severity: Low**

Go reloads config on every loop iteration (before each backup), which means config changes take effect within one watch_interval. Rust only reloads when explicitly signaled (SIGHUP or API). This means config changes in Rust require an explicit trigger, while Go picks them up automatically.

This is an intentional design difference (Rust design doc section 10.8 specifies signal-based reload).

### Gap: VALIDATE AFTER RELOAD
**Severity: Low**

Go calls `ValidateWatchParams()` after reload, which can return an error that **terminates the watch loop** (e.g., if `fullInterval <= watchInterval`). Rust validates but only logs a warning and keeps the old config on failure. This is arguably safer behavior from Rust.

---

## 6. Error Recovery (Consecutive Errors, Force Full)

### Go
- Tracks `createRemoteErrCount` and `deleteLocalErrCount` **separately**.
- Error count resets to 0 on success for each category independently.
- Abort condition: `createRemoteErrCount > BackupsToKeepRemote` OR `deleteLocalErrCount > BackupsToKeepLocal`.
- Secondary abort: if ANY error AND `time.Since(lastFullBackup) > FullDuration`, aborts (errors spanning an entire full interval).
- On error: does NOT set force_next_full. Simply does not update prevBackupName.
- **No sleep on error** -- immediately retries on next loop iteration.

### Rust
- Tracks a single `consecutive_errors` counter.
- Error count resets to 0 on ANY successful cycle.
- Abort condition: `consecutive_errors >= max_consecutive_errors` (config value, default 5, 0 = unlimited).
- On error: sets `force_next_full = true` and increments `consecutive_errors`.
- **Sleeps retry_interval on error** before retrying.

### Gap: SEPARATE vs UNIFIED ERROR TRACKING
**Severity: Medium**

Go tracks create and delete errors separately, with thresholds tied to retention counts (BackupsToKeepRemote/Local). This means:
- If BackupsToKeepRemote=10, Go tolerates up to 10 consecutive create failures before aborting.
- If BackupsToKeepLocal=3, Go tolerates up to 3 consecutive delete failures.

Rust uses a single counter with a fixed threshold (max_consecutive_errors=5 by default). This is simpler but handles the abort condition differently.

### Gap: NO RETRY SLEEP IN GO
**Severity: Low**

Go has no retry_interval concept for watch errors. On failure, it immediately loops back and retries (possibly with a new backup name since time has changed). Rust sleeps for retry_interval (default 5m) before retrying. Rust's approach is more network-friendly but less responsive.

### Gap: TIME-BASED ABORT IN GO
**Severity: Medium**

Go has a time-based abort condition: if errors persist throughout an entire `fullInterval` (24h by default), the watch loop terminates. Rust does not have this -- it only checks `max_consecutive_errors`. A scenario with intermittent failures (success every few cycles) could run forever in Rust but might abort in Go if the pattern spans a full interval with at least one error active.

### Gap: DELETE ERROR IS SEPARATE IN GO
**Severity: Low**

Go tracks `deleteLocalErr` separately and includes it in the abort condition. In Rust, local deletion errors are logged as warnings and do NOT affect `consecutive_errors`. This means Rust is more tolerant of local deletion failures.

---

## 7. Watch Metrics (Prometheus)

### Go
Go does NOT have watch-specific metrics (no watch_state, no watch_last_full_timestamp, etc.). Instead:
- `ExecuteWithMetrics()` wraps create_remote and delete operations, updating the general `SuccessfulCounter`/`FailedCounter`/`LastStart`/`LastFinish`/`LastDuration`/`LastStatus` for those commands.
- After each cycle, updates `LastBackupSizeRemote` and `NumberBackupsRemote` by listing remote backups.
- Metrics namespace is `clickhouse_backup_`.

### Rust
Rust has dedicated watch metrics:
- `chbackup_watch_state` (IntGauge, 0-7)
- `chbackup_watch_last_full_timestamp` (Gauge)
- `chbackup_watch_last_incremental_timestamp` (Gauge)
- `chbackup_watch_consecutive_errors` (IntGauge)
- General metrics namespace is `chbackup_`.

### Gap: METRIC NAMES DIFFER
**Severity: Low (expected)**

Go uses `clickhouse_backup_` prefix; Rust uses `chbackup_`. This is expected for a replacement tool. Monitoring dashboards would need updating.

### Gap: Rust HAS MORE WATCH METRICS
**Severity: None (Rust superset)**

Rust's watch_state, watch_last_full_timestamp, watch_last_incremental_timestamp, and watch_consecutive_errors are additions not present in Go. This is strictly additional functionality.

### Gap: PER-CYCLE REMOTE SIZE/COUNT UPDATE
**Severity: Low**

Go updates `LastBackupSizeRemote` and `NumberBackupsRemote` after every watch cycle (inside the loop). Rust updates backup counts only on `/metrics` scrape (via `refresh_backup_counts()`). This means Rust metrics are only updated when Prometheus scrapes, while Go metrics are updated in real-time after each backup.

---

## 8. Watch API Integration

### Go
- `POST/GET /backup/watch` endpoint starts a new watch loop via goroutine.
- Accepts many query parameters: `watch_interval`, `full_interval`, `watch_backup_name_template`, `table`, `partitions`, `schema`, `rbac`, `configs`, `named-collections`, `skip_check_parts_columns`, `delete_source`, `skip_projections`.
- No separate stop/status endpoints. Stopping is via `/backup/kill?command=watch`.
- `handleWatchResponse()` handles exit: if `WatchIsMainProcess` and error is not `context.Canceled`, sends stop signal to server.
- Watch parameters are passed as arguments to `Watch()`, NOT from config.

### Rust
- `POST /api/v1/watch/start` -- dedicated start endpoint.
- `POST /api/v1/watch/stop` -- dedicated stop endpoint.
- `GET /api/v1/watch/status` -- returns watch state, errors, timestamps.
- `POST /api/v1/reload` -- triggers config reload.
- Parameters come from config, not from the API request.
- `handleWatchResponse` equivalent is in the tokio::spawn callback in `start_server()`.

### Gap: WATCH API PARAMETERS
**Severity: Medium**

Go's `/backup/watch` endpoint accepts many parameters (tables, partitions, schema, rbac, configs, named_collections, skip_check_parts_columns, delete_source, skip_projections, intervals, template). These OVERRIDE config values for that watch session.

Rust's `/api/v1/watch/start` accepts NO parameters -- it uses the current config values. To change watch behavior, you must update the config file and reload. This is less flexible for API-driven control.

### Gap: WATCH API ENDPOINT PATH
**Severity: Low**

Go uses `/backup/watch` (single endpoint, GET or POST). Rust uses `/api/v1/watch/start`, `/api/v1/watch/stop`, `/api/v1/watch/status` (three separate endpoints). This is a deliberate API redesign, not a bug.

### Gap: WATCH STOP VIA KILL IN GO
**Severity: Low**

Go stops watch via the generic `/backup/kill?command=watch` endpoint. Rust has a dedicated `/api/v1/watch/stop`. Functionally equivalent.

---

## 9. Shutdown Handling

### Go
- `ctx.Done()` checked at the top of each loop iteration.
- `handleWatchResponse()`: if `WatchIsMainProcess` is true and error is NOT `context.Canceled`, sends stop signal via `api.stop <- struct{}{}`.
- If error IS `context.Canceled` (user-initiated kill), does NOT stop server.

### Rust
- Shutdown signal via `tokio::sync::watch::Receiver<bool>` checked in `interruptible_sleep()`.
- On watch loop exit: if `watch_is_main_process` and exit reason is NOT `Shutdown` or `Stopped`, calls `std::process::exit(0)`.
- Explicit `WatchLoopExit` enum: `Shutdown`, `MaxErrors`, `Stopped`.

### Gap: EXIT CODE ON WATCH_IS_MAIN_PROCESS
**Severity: Low**

Rust calls `std::process::exit(0)` (success code) when watch_is_main_process triggers server termination. Go sends a stop signal to the server's stop channel. The Go approach allows the server to clean up (drop integration tables, etc.) while Rust's `exit(0)` is abrupt. However, the Rust code currently uses `exit(0)` not `exit(1)`, whereas the CLAUDE.md says it should call `exit(1)`.

**Discrepancy in Rust code:** The CLAUDE.md states "the server process calls `std::process::exit(1)`" but the actual code at `src/server/mod.rs:207` calls `std::process::exit(0)`. This should be reconciled.

---

## 10. Additional Go Features Not in Rust

### Go: CreateToRemote (combined create+upload)
Go's watch loop calls `b.CreateToRemote()` which is a combined create+upload operation. Rust calls `backup::create()` and `upload::upload()` sequentially. Functionally equivalent but Go's is atomic from an error-handling perspective.

### Go: Local Backup Deletion Logic
Go's deletion logic is:
```go
if !deleteSource && b.cfg.General.BackupsToKeepLocal >= 0 {
    b.RemoveBackupLocal(ctx, backupName, nil)
}
```
This respects `BackupsToKeepLocal`:
- `BackupsToKeepLocal == -1` means "delete in upload step" (handled by CreateToRemote).
- `BackupsToKeepLocal >= 0` means "delete explicitly after create_remote".

Rust's logic:
```rust
if ctx.config.watch.delete_local_after_upload {
    list::delete_local(&data_path, &name_clone)
}
```
Uses a separate `delete_local_after_upload` boolean config. Similar but different config semantics.

### Go: ClickHouse Connection Per-Cycle
Go opens/closes ClickHouse connection per cycle (`b.ch.Connect()` / `b.ch.Close()`). Rust keeps a persistent connection. Go's approach is more resilient to ClickHouse restarts but adds overhead.

---

## Summary of Gaps by Severity

### High Severity
None.

### Medium Severity
1. **Backup type string mismatch**: Go uses `"increment"`, Rust uses `"incr"`. Affects template resolution and resume matching.
2. **Separate vs unified error tracking**: Go tracks create/delete errors independently with retention-based thresholds. Rust uses a single counter.
3. **Time-based abort**: Go aborts if errors persist through a full interval. Rust does not have this.
4. **Watch API parameters**: Go's watch API accepts runtime parameter overrides. Rust does not.

### Low Severity
5. **Force-full-on-error**: Rust forces full after error; Go retries same type.
6. **No retry sleep in Go**: Go retries immediately; Rust waits retry_interval.
7. **Template matching**: Go uses regex; Rust uses prefix matching.
8. **Macros queried once**: Rust caches; Go queries every cycle.
9. **No template uniqueness validation**: Rust does not require `{time:...}`.
10. **Config reload timing**: Go reloads every cycle; Rust requires explicit signal.
11. **Per-cycle metric updates**: Go updates remote size/count in loop; Rust on scrape only.
12. **Exit code**: Rust `exit(0)` vs documented `exit(1)` for watch_is_main_process.
13. **Delete error tracking**: Go includes delete errors in abort condition; Rust does not.

### Informational (design differences, not bugs)
- Time format syntax (Go format vs strftime) -- expected for different languages.
- Metric names (`clickhouse_backup_` vs `chbackup_`) -- expected for replacement tool.
- API endpoint paths (`/backup/watch` vs `/api/v1/watch/*`) -- deliberate redesign.
- Rust has more watch metrics (watch_state, timestamps, errors).
- Rust uses resume_state per-iteration vs Go's one-time calculatePrev.
