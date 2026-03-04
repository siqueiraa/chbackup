# Plan: Fix P1/P2 Audit Findings in chbackup

## Goal

Fix 5 correctness issues discovered during audit: 2 P1 (lock bypass on shortcut names, backup name collision) and 3 P2 (restore flag mutual exclusion, latest/previous sort inconsistency, create --resume doc mismatch). One finding (P2-D doctests) confirmed resolved; one finding (P3 restart) deferred.

## Architecture Overview

This plan modifies 5 existing source files and 1 documentation file. No new modules, structs, or public API are introduced. All changes are surgical bug fixes to existing functions.

**Files modified:**
- `src/cli.rs` -- Add clap `conflicts_with` for `--schema` vs `--data-only`
- `src/list.rs` -- Change `resolve_backup_shortcut` sort to use timestamp instead of relying on name order
- `src/main.rs` -- Move lock acquisition after shortcut resolution; restructure command dispatch
- `src/backup/mod.rs` -- Replace `create_dir_all` with pre-existence check + `create_dir`
- `docs/design.md` -- Add note that create `--resume` is deferred

## Architecture Assumptions (VALIDATED)

### Component Ownership

- **Lock lifecycle**: Created in `main.rs:run()`. `PidLock::acquire()` uses `O_CREAT|O_EXCL` (atomic). Lock released on Drop.
- **Shortcut resolution**: Done inside each command branch AFTER lock. `resolve_local_shortcut` requires only `data_path` (from config). `resolve_remote_shortcut` requires `S3Client` (created inside command branch).
- **Config availability**: `Config::load()` at line 105, BEFORE lock at line 128. `config.clickhouse.data_path` is available early.
- **S3Client creation**: Inside command branches (lines 207, 237, 313). NOT available before lock.
- **Backup directory creation**: Single call site in `backup::create()` at `backup/mod.rs:286`. All callers (CLI, API, watch) go through this function.

### What This Plan CANNOT Do

- **Move remote shortcut resolution before lock**: `S3Client::new()` requires async I/O and config, happens inside command branches. Would require creating S3 client outside the match or restructuring the entire flow.
- **Add subsecond precision to auto-names globally**: Would break backward compatibility with existing backups and watch template matching.
- **Remove `--resume` from create CLI**: Existing scripts may pass `--resume` with `create` and expect success (no-op).

### Fix Strategy for P1-A (Lock Bypass)

The lock is currently acquired at line 128 with the raw CLI name (e.g., "latest"). The resolved name (e.g., "2024-02-15T100000") is determined later inside each command branch. This means:
1. Two concurrent `upload latest` commands lock `/tmp/chbackup.latest.pid` -- serialized correctly.
2. But `upload latest` and `upload 2024-02-15T100000` lock different files -- no mutual exclusion.

**Chosen approach**: Move lock acquisition into each command branch, AFTER shortcut resolution. Extract a helper function `acquire_lock(command, backup_name) -> Option<PidLock>` to reduce duplication. This ensures the lock is always taken on the resolved (actual) backup name.

For commands using local shortcuts (upload, restore): resolution is cheap (filesystem scan), config is available.
For commands using remote shortcuts (download, restore_remote): S3Client is needed, so lock moves after S3Client creation and shortcut resolution.
For delete: already has both local and remote resolution paths inside the match arm.
For commands that don't use shortcuts (create, create_remote): lock uses the provided/generated name directly -- no change in behavior.

## Risk Assessment

| Risk | Level | Mitigation |
|------|-------|------------|
| Lock restructuring breaks existing lock semantics | YELLOW | Test that non-shortcut names still lock correctly; existing `lock.rs` unit tests cover scope mapping |
| `create_dir` vs `create_dir_all` breaks first-time backup | GREEN | Parent directory `{data_path}/backup/` is created during first backup or already exists; `create_dir` only needs the leaf |
| Timestamp sort for `resolve_backup_shortcut` changes behavior | GREEN | For date-based auto-names, timestamp sort == lexicographic sort. Only custom-named backups see different behavior, and timestamp sort is the correct semantic |
| `conflicts_with` on schema/data_only breaks existing scripts | GREEN | No legitimate use case for both flags together; combined use was already a silent no-op (bug) |
| Design doc edit introduces formatting issues | GREEN | Single line addition with note |

## Expected Runtime Logs

| Pattern | Required | Description |
|---------|----------|-------------|
| `Acquiring lock` | yes | Lock acquisition log (already exists, should now show resolved name) |
| `Lock acquired` | yes | Successful lock acquisition |
| `Resolved local backup name shortcut` | conditional | Only when "latest"/"previous" used with local commands |
| `Resolved remote backup name shortcut` | conditional | Only when "latest"/"previous" used with remote commands |
| `ERROR: backup.*already exists` | conditional | When backup name collision is detected |

**Note:** These are existing log patterns. This plan adds no new DEBUG_VERIFY markers because all fixes are correctness changes to existing flows, not new features. Runtime verification is covered by behavioral tests.

## Known Related Issues (Out of Scope)

| Issue | Reason | Future Plan |
|-------|--------|-------------|
| P3: Roadmap restart mismatch | Low priority, server restart semantics are correct for ArcSwap pattern | Future polish phase |
| P2-D: Doctests | Confirmed PASSING in current codebase | No action needed |
| API shortcut resolution | Server API endpoints don't use shortcuts (name passed directly) | N/A -- different code path |
| Watch mode name collision | Watch uses templates with second-precision; collision window exists but is mitigated by watch interval | Future: add collision check in watch loop |

## Dependency Groups

```
Group A (Independent - P2 fixes):
  - Task 1: Fix P2-A (restore mutual exclusion) -- cli.rs only
  - Task 2: Fix P2-C (latest/previous sort) -- list.rs only
  - Task 5: Fix P2-B (design doc --resume note) -- docs/design.md only

Group B (Sequential - P1 fixes):
  - Task 3: Fix P1-A (lock bypass on shortcuts) -- main.rs restructure
  - Task 4: Fix P1-B (backup name collision) -- backup/mod.rs

Group C (Final):
  - Task 6: Verify doctests pass (P2-D confirmation)
```

## Tasks

### Task 1: Fix P2-A -- Restore mutual exclusion for --schema and --data-only

**Problem:** CLI accepts both `--schema` and `--data-only` flags simultaneously. Combined use causes a silent no-op (no schema created, no data attached).

**Fix:** Add `conflicts_with = "data_only"` attribute to the `schema` field in `Command::Restore` in `cli.rs`.

**TDD Steps:**
1. Write failing test: `test_restore_schema_and_data_only_conflict`
   - Use `clap::Command::try_get_matches_from()` to verify that passing both `--schema` and `--data-only` returns an `Err`
   - Verify the error message mentions the conflict
2. Implement: Add `#[arg(long, conflicts_with = "data_only")]` to the `schema` field in `cli.rs:148`
3. Verify test passes
4. Verify `cargo test` passes (no regressions)

**Files:** `src/cli.rs`
**Acceptance:** F001

**Notes:**
- The `conflicts_with` value must reference the field name `"data_only"` (not `"data-only"` -- clap maps long names to field names for conflict resolution)
- Existing pattern in codebase: no `conflicts_with` attributes currently exist, but this is standard clap derive API
- The `RestoreRemote` command does NOT have `--schema` or `--data-only` flags (per design), so no change needed there

---

### Task 2: Fix P2-C -- Sort latest/previous resolution by timestamp

**Problem:** `resolve_backup_shortcut` in `list.rs` relies on the input list being sorted by name (lexicographic). But retention functions sort by timestamp. For custom-named backups, lexicographic order may not match chronological order, causing "latest" to return the wrong backup.

**Fix:** Change `resolve_backup_shortcut` to explicitly sort the valid (non-broken) backups by timestamp before selecting the last/second-to-last entry. Backups with `None` timestamps are sorted to the beginning (oldest).

**TDD Steps:**
1. Write failing test: `test_resolve_backup_shortcut_sorts_by_timestamp`
   - Create 3 `BackupSummary` entries with names in alphabetical order but timestamps in reverse order
   - e.g., name="alpha" timestamp=2024-03-01, name="beta" timestamp=2024-01-01, name="gamma" timestamp=2024-02-01
   - Input list sorted by name: [alpha, beta, gamma]
   - "latest" should resolve to "alpha" (most recent timestamp), NOT "gamma" (last by name)
   - "previous" should resolve to "gamma" (second-most-recent timestamp)
2. Implement: In `resolve_backup_shortcut`, after filtering broken backups, sort `valid` by `b.timestamp` ascending, with `None` timestamps sorting first (before all `Some` values)
3. Update existing tests: add timestamps to existing test data (existing tests use `timestamp: None` which is unrealistic for non-broken backups)
4. Verify all shortcut tests pass

**Files:** `src/list.rs`
**Acceptance:** F002

**Implementation detail:**

Sort BEFORE the `match name` so both `latest` and `previous` branches use the same timestamp-ordered list. Do NOT sort inside each branch separately — that would leave `previous` using lexicographic order if missed:
```rust
pub fn resolve_backup_shortcut(name: &str, backups: &[BackupSummary]) -> Result<String> {
    let mut valid: Vec<&BackupSummary> = backups.iter().filter(|b| !b.is_broken).collect();
    valid.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    // valid is now sorted by timestamp ascending; None timestamps sort first (oldest)
    match name {
        "latest" => valid
            .last()
            .map(|b| b.name.clone())
            .ok_or_else(|| anyhow::anyhow!("No backups found to resolve 'latest'")),
        "previous" => {
            if valid.len() < 2 {
                anyhow::bail!(
                    "Not enough backups for 'previous' (found {} valid backups)",
                    valid.len()
                );
            }
            Ok(valid[valid.len() - 2].name.clone())
        }
        _ => Ok(name.to_string()),
    }
}
```
`Option<DateTime<Utc>>` sorts `None < Some(_)`, and `DateTime<Utc>` sorts chronologically. After the sort, `valid.last()` is the most recent timestamp and `valid[valid.len()-2]` is the second-most-recent — correct for both shortcuts.

---

### Task 3: Fix P1-A -- Move lock acquisition after shortcut resolution

**Problem:** Lock acquired at `main.rs:128` uses raw CLI name (e.g., "latest"). After shortcut resolution inside command branches, the actual backup name may differ. Two concurrent operations on the same backup -- one via shortcut, one via direct name -- would not be mutually excluded.

**Fix:** Remove the early lock acquisition block (lines 115-146). Instead, acquire the lock inside each command branch AFTER shortcut resolution. Extract a helper function to reduce code duplication.

**TDD Steps:**
1. Write unit test: `test_lock_uses_resolved_name`
   - This is a code review / structural verification test
   - Verify that `lock_for_command` is NOT called before the `match cli.command` block
   - Verify that each command branch that needs a lock calls the acquire helper after name resolution
2. Implement:
   a. Add a helper function in `main.rs`:
      ```rust
      fn acquire_backup_lock(cmd_name: &str, backup_name: &str) -> Result<Option<PidLock>> {
          let scope = lock_for_command(cmd_name, Some(backup_name));
          match lock_path_for_scope(&scope) {
              Some(ref path) => {
                  info!(command = cmd_name, lock_path = %path.display(), "Acquiring lock");
                  let guard = PidLock::acquire(path, cmd_name)?;
                  info!("Lock acquired");
                  Ok(Some(guard))
              }
              None => Ok(None),
          }
      }
      ```
   b. Remove the early lock block from `main.rs`, BUT keep lines 119-126 (`validate_backup_name` security check — prevents path-traversal lock file creation). Specifically: remove lines 115-118 (`cmd_name`/`bak_name` extraction) and lines 128-146 (lock acquisition). Lines 119-126 stay in place before the `match`.
   c. In each command branch, call `acquire_backup_lock` after shortcut resolution:
      - `Create`: after `resolve_backup_name()` -- `let _lock = acquire_backup_lock("create", &name)?;`
      - `Upload`: after `resolve_local_shortcut()` -- `let _lock = acquire_backup_lock("upload", &name)?;`
      - `Download`: after `resolve_remote_shortcut()` -- `let _lock = acquire_backup_lock("download", &name)?;`
      - `Restore`: after `resolve_local_shortcut()` -- `let _lock = acquire_backup_lock("restore", &name)?;`
      - `CreateRemote`: after `resolve_backup_name()` -- `let _lock = acquire_backup_lock("create_remote", &name)?;`
      - `RestoreRemote`: after `resolve_remote_shortcut()` -- `let _lock = acquire_backup_lock("restore_remote", &name)?;`
      - `Delete`: after shortcut resolution -- `let _lock = acquire_backup_lock("delete", &name)?;`
        (Note: `lock_for_command("delete", Some(name))` returns `LockScope::Global` by design in `lock.rs:148` — delete is always global-scoped regardless of backup name. The call still goes through `acquire_backup_lock` for consistency; the resulting lock path will be `/tmp/chbackup.global.pid`.)
      - `Clean`: `let _lock = acquire_global_lock("clean")?;`
      - `CleanBroken`: `let _lock = acquire_global_lock("clean_broken")?;`
   d. For commands with `LockScope::None` (list, tables, etc.): no lock needed, no change
   e. Keep the `validate_backup_name` check before lock (it already runs on the raw CLI name, which is correct -- validates path traversal before any filesystem operation)
3. Verify `cargo check` passes
4. Verify `cargo test` passes

**Files:** `src/main.rs`
**Acceptance:** F003

**Notes:**
- The `_lock_guard` variable binding must stay alive for the duration of the command execution (Rust RAII pattern). The `_lock` binding in each branch achieves this.
- For `Clean` and `CleanBroken` which use `LockScope::Global`, add a small helper or inline the global lock pattern:
  ```rust
  fn acquire_global_lock(cmd_name: &str) -> Result<Option<PidLock>> {
      let scope = lock_for_command(cmd_name, None);
      // ... same pattern as acquire_backup_lock
  }
  ```
- The `validate_backup_name` block at lines 119-126 should remain BEFORE the match, as it validates the raw CLI input before any processing. It does NOT need to see the resolved name since "latest" and "previous" are valid strings that pass validation.
- The `bak_name` and `cmd_name` extraction at lines 116-117 can be removed or moved, since each branch already destructures the command.

**Edge cases:**
- Commands with no backup name (list, tables, etc.): no lock needed, handled by match arm having no lock call
- Commands that generate a name (create): lock after name generation
- Commands with global lock (clean, clean_broken): separate path using `lock_for_command(cmd_name, None)` which returns `LockScope::Global`

---

### Task 4: Fix P1-B -- Backup name collision detection

**Problem:** `backup::create()` uses `create_dir_all(&backup_dir)` at `backup/mod.rs:286`, which succeeds silently if the directory already exists. Auto-generated names use second-precision timestamps, so two creates within the same second collide.

**Fix:** Check if the backup directory already exists before creating it. Use `create_dir` (fails if exists) instead of `create_dir_all`, after ensuring the parent `backup/` directory exists.

**TDD Steps:**
1. Write unit test: `test_create_backup_dir_rejects_existing`
   - Create a temp directory, create `{tmp}/backup/test-name/` manually
   - Call the equivalent check logic and verify it returns an error containing "already exists"
2. Implement in `backup/mod.rs` at line 286 (where `backup_dir` is already defined on line 282 as `PathBuf::from(&config.clickhouse.data_path).join("backup").join(backup_name)`):
   ```rust
   // 8. Create backup directory (fail if it already exists to prevent collision)
   // backup_dir is already computed at line 282 -- reuse it here.
   // First ensure the parent backup/ dir exists, then create the leaf with create_dir
   // (which fails atomically if the directory was already created by a concurrent run).
   let backup_parent = backup_dir.parent().expect("backup_dir always has a parent");
   std::fs::create_dir_all(backup_parent).with_context(|| {
       format!("Failed to create backup parent directory: {}", backup_parent.display())
   })?;
   if backup_dir.exists() {
       bail!("backup '{}' already exists at {}", backup_name, backup_dir.display());
   }
   std::fs::create_dir(&backup_dir).with_context(|| {
       format!("Failed to create backup directory: {}", backup_dir.display())
   })?;
   ```
3. Verify test passes
4. Verify `cargo test` passes (no regressions)

**Files:** `src/backup/mod.rs`
**Acceptance:** F004

**Notes:**
- There is a TOCTOU gap between `exists()` and `create_dir()`. However, `create_dir` itself will fail if the directory was created between the check and the call, so the explicit `exists()` check provides a better error message. The TOCTOU gap is acceptable because:
  - PidLock already serializes same-name operations (after Task 3 fix)
  - The check is defense-in-depth for the case where names collide naturally (same-second auto-generation)
- `create_dir_all` was needed to create the parent `backup/` directory. We split this: `create_dir_all` for parent, `create_dir` for leaf.
- Watch mode callers: the watch loop generates unique names via templates with second-precision timestamps. Collision is theoretically possible but mitigated by the watch interval (typically 60s+). The collision detection is still correct behavior -- the watch loop will see the error and retry with the next cycle.
- API callers: the server API uses the same `backup::create()` entry point, so they get collision detection for free.

---

### Task 5: Fix P2-B -- Update design doc for create --resume

**Problem:** Design doc line 919 lists `--resume` for `create, upload, download, restore`. But `create --resume` is intentionally deferred (logs "has no effect" warning). The design doc creates a false expectation.

**Fix:** Add a note to the design doc flag table clarifying that `--resume` for `create` is not yet implemented.

**TDD Steps:**
1. Read current design doc line 919 to verify exact format
2. Edit: Change the `--resume` row to exclude `create` or add a footnote
3. Verify no formatting issues

**Files:** `docs/design.md`
**Acceptance:** F005

**Implementation detail:**
Change line 919 from:
```
| `--resume` | create, upload, download, restore | Resume interrupted operation from state file |
```
To:
```
| `--resume` | upload, download, restore | Resume interrupted operation from state file (create: deferred -- local-only operation, no remote state to resume from) |
```

---

### Task 6: Verify doctests pass (P2-D confirmation)

**Problem:** Audit reported doctests might fail with `unresolved import chbackup::path_encoding`. Discovery agent confirmed doctests PASS in the current codebase (`cargo test --doc path_encoding`).

**TDD Steps:**
1. Run `cargo test --doc path_encoding` and verify PASS
2. Run `cargo test --doc` for full doctest coverage
3. Document result in SESSION.md

**Files:** (none -- verification only)
**Acceptance:** F006

---

## Consistency Validation Results

| Check | Status | Notes |
|-------|--------|-------|
| RC-006 (Verified APIs) | PASS | All functions verified: `lock_for_command`, `lock_path_for_scope`, `PidLock::acquire`, `resolve_backup_shortcut`, `resolve_local_shortcut`, `resolve_remote_shortcut`, `validate_backup_name`, `create_dir_all`, `create_dir`, `list_local`, `list_remote` |
| RC-008 (TDD sequencing) | PASS | No task uses fields/functions from later tasks. Task 3 and Task 4 are independent (different files). |
| RC-015 (Cross-task types) | PASS | No data flows between tasks. Each task modifies independent code. |
| RC-016 (Struct completeness) | PASS | No new structs defined. |
| RC-017 (State fields) | PASS | No new state fields. |
| RC-018 (Test steps) | PASS | Each task has explicit test names, inputs, and assertions. |
| RC-019 (Existing patterns) | PASS | Lock helper follows existing `PidLock::acquire` pattern. `conflicts_with` is standard clap. |
| RC-021 (File locations) | PASS | All file locations verified: cli.rs:148 (schema field), list.rs:312 (resolve_backup_shortcut), main.rs:128 (lock), backup/mod.rs:286 (create_dir_all) |
| RC-035 (cargo fmt) | PASS | Will run cargo fmt as part of each task |

## Notes

**Phase 4.5 (Interface Skeleton) SKIPPED**: This plan modifies existing functions only. No new imports, no new types, no new public API. All changes are within existing function bodies or clap derive attributes. Compilation verification is done via `cargo check` in each task's TDD steps.

**CLAUDE.md update task SKIPPED**: Per `context/affected-modules.json`, no module CLAUDE.md files need updating. All changes are bug fixes to existing code with no structural changes or new patterns.
