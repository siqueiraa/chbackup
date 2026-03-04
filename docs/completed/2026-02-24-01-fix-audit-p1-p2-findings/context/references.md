# Symbol and Reference Analysis

## Phase 1: Symbol Verification

All symbols verified via LSP hover and direct file reads.

### resolve_backup_shortcut (list.rs:312)

**Signature:**
```rust
pub fn resolve_backup_shortcut(name: &str, backups: &[BackupSummary]) -> Result<String>
```

**References (10 total across 2 files):**
- `src/list.rs:2662, 2713, 2720, 2727, 2749, 2805, 2809` (unit tests)
- `src/main.rs:733` (inside `resolve_local_shortcut`)
- `src/main.rs:748` (inside `resolve_remote_shortcut`)

**Behavior:** Takes a presorted `&[BackupSummary]`, filters out broken backups, then:
- "latest" -> `valid.last()` (relies on input sort order)
- "previous" -> `valid[valid.len() - 2]` (relies on input sort order)

**Sort order concern:** `list_local()` (line 373) and `list_remote()` (line 468) sort by `a.name.cmp(&b.name)` (lexicographic). But `retention_local()` (line 752) and `retention_remote()` (line 1085) sort by `a.timestamp.cmp(&b.timestamp)`. For auto-generated date-based names (`YYYY-MM-DDTHHMMSS`), lexicographic == chronological. For custom-named backups (e.g., `alpha`, `beta`), they can diverge.

### lock_for_command (lock.rs:140)

**Signature:**
```rust
pub fn lock_for_command(command: &str, backup_name: Option<&str>) -> LockScope
```

**References (16 total across 2 files):**
- `src/lock.rs:293-319` (unit tests, 14 refs)
- `src/main.rs:10` (import)
- `src/main.rs:128` (call site -- THE critical location for P1 fix)

**Behavior:** Maps command name + optional backup name to a `LockScope`. When `backup_name` is `Some("latest")`, returns `LockScope::Backup("latest")` -- which locks `/tmp/chbackup.latest.pid`, NOT the actual backup being operated on.

### lock_path_for_scope (lock.rs:157)

**Signature:**
```rust
pub fn lock_path_for_scope(scope: &LockScope) -> Option<PathBuf>
```

**References (3 total across 2 files):**
- `src/main.rs:10` (import)
- `src/main.rs:129` (call site)
- `src/lock.rs:157` (definition)

### backup::create (backup/mod.rs:93)

**Signature:**
```rust
pub async fn create(
    config: &Config, ch: &ChClient, backup_name: &str,
    table_pattern: Option<&str>, schema_only: bool, diff_from: Option<&str>,
    partitions: Option<&str>, skip_check_parts_columns: bool,
    rbac: bool, configs: bool, named_collections: bool,
    skip_projections: &[String],
) -> Result<BackupManifest>
```

**References (8 total across 4 files):**
- `src/main.rs:180` (CLI create command)
- `src/main.rs:322` (CLI create_remote command)
- `src/server/routes.rs:333` (API create action)
- `src/server/routes.rs:411` (API create_remote action)
- `src/server/routes.rs:715` (standalone create endpoint)
- `src/server/routes.rs:976` (standalone create_remote endpoint)
- `src/watch/mod.rs:416` (watch loop)
- `src/backup/mod.rs:93` (definition)

**Name collision point:** Line 286 of backup/mod.rs uses `create_dir_all(&backup_dir)` which succeeds silently if directory exists. No pre-existence check.

### resolve_local_shortcut (main.rs:730)

**Signature:**
```rust
fn resolve_local_shortcut(name: &str, data_path: &str) -> Result<String>
```

**References (4 in main.rs):**
- Line 206: Upload command branch (AFTER lock acquisition at line 128)
- Line 268: Restore command branch (AFTER lock acquisition at line 128)
- Line 524: Delete command branch (AFTER lock acquisition at line 128)
- Line 730: Definition

### resolve_remote_shortcut (main.rs:745)

**Signature:**
```rust
async fn resolve_remote_shortcut(name: &str, s3: &S3Client) -> Result<String>
```

**References (4 in main.rs):**
- Line 238: Download command branch (AFTER lock acquisition at line 128)
- Line 376: RestoreRemote command branch (AFTER lock acquisition at line 128)
- Line 526: Delete command branch (AFTER lock acquisition at line 128)
- Line 745: Definition

## Phase 1.5: Call Hierarchy Analysis (LSP)

### Lock Acquisition Flow

```
main.rs:run()
  Line 117: cmd_name = command_name(&cli.command)
  Line 118: bak_name = backup_name_from_command(&cli.command)  // Raw CLI arg, e.g. "latest"
  Line 128: scope = lock_for_command(cmd_name, bak_name)       // Locks "latest", NOT real name
  Line 129: lock_file_path = lock_path_for_scope(&scope)       // /tmp/chbackup.latest.pid
  Line 131-146: _lock_guard = PidLock::acquire(path, cmd_name)

  ... then inside each command branch:
  Line 206: name = resolve_local_shortcut(&raw_name, ...)       // Resolves to real name
  Line 238: name = resolve_remote_shortcut(&raw_name, ...)      // Resolves to real name
```

**Problem:** Lock acquired with unresolved shortcut name. Two concurrent commands using "latest" both lock `/tmp/chbackup.latest.pid` -- one fails. But a concurrent command with the actual backup name (e.g., `2024-02-15T100000`) bypasses the lock because it locks `/tmp/chbackup.2024-02-15T100000.pid`.

### backup::create Callers Impact for Name Collision Fix

| Caller | File | Line | Name Generation |
|--------|------|------|-----------------|
| CLI create | main.rs | 180 | `resolve_backup_name()` -- auto or user-provided |
| CLI create_remote | main.rs | 322 | `resolve_backup_name()` -- auto or user-provided |
| API create action | routes.rs | 333 | `Utc::now().format("%Y-%m-%dT%H%M%S")` (same format) |
| API create_remote | routes.rs | 411 | Same as above |
| API create endpoint | routes.rs | 715 | From request body `name` field |
| API create_remote endpoint | routes.rs | 976 | From request body `name` field |
| Watch loop | watch/mod.rs | 416 | `resolve_name_template()` with macros |

**All callers** feed the name into `backup::create()` at `backup_name` parameter. The pre-existence check should be inside `backup::create()` itself (single enforcement point), NOT at each call site.

### BackupSummary.timestamp Type

```rust
pub timestamp: Option<DateTime<Utc>>
```

Verified via LSP hover. The `Option` wrapper means broken backups have `timestamp: None`. In the `resolve_backup_shortcut` function, broken backups are filtered out before sort, so `None` timestamps are never compared. But if we change to sort by timestamp within `resolve_backup_shortcut`, we need to handle the `None` case for non-broken backups that somehow have no timestamp.

## Phase 2: Cross-Reference Verification

### P1 Lock Bypass -- Affected Commands

Commands that use shortcuts AND take backup-scoped locks:
| Command | Shortcut Type | Lock Before Resolve | Resolution Site |
|---------|--------------|--------------------|----|
| upload | local (`resolve_local_shortcut`) | Yes (line 128) | main.rs:206 |
| download | remote (`resolve_remote_shortcut`) | Yes (line 128) | main.rs:238 |
| restore | local (`resolve_local_shortcut`) | Yes (line 128) | main.rs:268 |
| restore_remote | remote (`resolve_remote_shortcut`) | Yes (line 128) | main.rs:376 |
| delete | local OR remote | Yes (line 128) | main.rs:522-526 |

Commands that DO NOT use shortcuts:
- `create`: generates name or uses explicit user-provided name
- `create_remote`: same as create

### P2 Schema + Data-Only Mutual Exclusion -- Restore Flow

When `schema_only=true, data_only=true`:
1. Phase 0 DROP: SKIPPED (`rm && !data_only` -> false)
2. Phase 1 CREATE databases: SKIPPED (`!data_only` -> false)
3. Phase 2 CREATE data tables: `create_tables()` called with `data_only=true` (internal guard skips DDL)
4. Schema-only branch (line 243): enters `if schema_only` -> returns `Ok(())` early
5. Net effect: **silent no-op** -- no schema created, no data attached

### P2 Create --resume -- Design vs Implementation

- Design doc line 919: `--resume` listed for `create, upload, download, restore`
- Implementation (main.rs:170-172): logs info message and ignores the flag
- Comment in code (main.rs:166-169): "explicitly deferred" with rationale
- `backup::create()` signature: NO resume parameter (12 parameters, none is resume)
- Existing commit: `3973d090 docs(main): clarify create --resume as intentionally deferred design decision`

The create command operates on local filesystem only (FREEZE + hardlink). There is no remote state to resume from. The design doc listing appears aspirational. The implementation correctly defers it. This is a documentation alignment issue, not a code bug.
