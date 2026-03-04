# Type Verification Table

## Verified Types

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `LockScope` | enum | `enum LockScope { Backup(String), Global, None }` | lock.rs:125-132 |
| `LockScope::Backup` | String variant | `Backup(String)` | lock.rs:127 |
| `PidLock` | struct | `struct PidLock { path: PathBuf }` | lock.rs:28-30 |
| `lock_for_command` return | LockScope | `fn(command: &str, backup_name: Option<&str>) -> LockScope` | lock.rs:140 |
| `lock_path_for_scope` return | Option<PathBuf> | `fn(scope: &LockScope) -> Option<PathBuf>` | lock.rs:157 |
| `validate_backup_name` | fn | `fn(name: &str) -> Result<(), &'static str>` | server/state.rs:389 |
| `backup_name_from_command` return | `Option<&str>` | `fn(cmd: &Command) -> Option<&str>` | main.rs:45 |
| `resolve_backup_name` return | `Result<String>` | `fn(name: Option<String>) -> Result<String>` | main.rs:683 |
| `backup_name_required` return | `Result<String>` | `fn(name: Option<String>, command: &str) -> Result<String>` | main.rs:696 |
| `resolve_local_shortcut` return | `Result<String>` | `fn(name: &str, data_path: &str) -> Result<String>` | main.rs:730 |
| `resolve_remote_shortcut` return | `Result<String>` | `async fn(name: &str, s3: &S3Client) -> Result<String>` | main.rs:745 |
| `resolve_backup_shortcut` return | `Result<String>` | `fn(name: &str, backups: &[BackupSummary]) -> Result<String>` | list.rs:312 |
| `BackupSummary.name` | String | `pub name: String` | list.rs:48 |
| `BackupSummary.timestamp` | Option<DateTime<Utc>> | `pub timestamp: Option<DateTime<Utc>>` | list.rs:50 |
| `BackupSummary.is_broken` | bool | `pub is_broken: bool` | list.rs:72 |
| `list_local` return | `Result<Vec<BackupSummary>>` | sorted by `a.name.cmp(&b.name)` | list.rs:339, 373 |
| `list_remote` return | `Result<Vec<BackupSummary>>` | sorted by `a.name.cmp(&b.name)` | list.rs:383, 468 |
| `Command::Restore.schema` | bool | `#[arg(long)] schema: bool` | cli.rs:149 |
| `Command::Restore.data_only` | bool | `#[arg(long = "data-only")] data_only: bool` | cli.rs:152 |
| `Command::Create.resume` | bool | `#[arg(long)] resume: bool` | cli.rs:91 |
| `backup::create` params | `(config, ch, name, ...)` | 12 params, no resume param | backup/mod.rs:93-106 |
| `restore::restore` params | `(config, ch, name, ..., schema_only, data_only, ...)` | 15 params | restore/mod.rs:80-96 |
| `std::fs::create_dir_all` | fn | succeeds if dir exists, creates parents | std lib |
| `encode_path_component` | fn | `pub fn(s: &str) -> String` | path_encoding.rs:29 |
| `sanitize_path_component` | fn | `pub fn(s: &str) -> String` | path_encoding.rs:61 |

## Architecture Assumptions (VALIDATED)

### Component Ownership

- **Lock lifecycle**: Created in `main.rs:run()` before command dispatch. `PidLock::acquire()` is atomic via `O_CREAT|O_EXCL`. Lock released on Drop.
- **Shortcut resolution**: Done inside each command branch AFTER lock acquisition. `resolve_local_shortcut` requires only `data_path` (from config). `resolve_remote_shortcut` requires `S3Client` (created inside command branch).
- **Backup name flow**: CLI arg -> `backup_name_from_command()` (raw) -> `validate_backup_name()` -> `lock_for_command()` (uses raw name) -> command branch -> shortcut resolution (to real name) -> actual operation.
- **Config availability**: `Config::load()` happens at line 105, BEFORE lock acquisition at line 128. So `config.clickhouse.data_path` is available for local shortcut resolution before locking.
- **S3Client creation**: Happens inside command branches (e.g., line 207, 237, 313). NOT available before lock.

### What This Plan CANNOT Do

- **Move remote shortcut resolution before lock**: `S3Client::new()` requires network I/O and happens inside command branches. Moving it before the lock would require either creating S3 client twice or restructuring the entire command dispatch.
- **Add subsecond precision to auto-names globally**: Changing `%Y-%m-%dT%H%M%S` format would break backward compatibility with existing backups and watch template matching.
- **Remove `--resume` from create CLI without breaking compatibility**: Existing scripts may use `--resume` with `create` and expect it to succeed (no-op).

### Fix Strategy for P1 Lock Bypass

The cleanest approach is to resolve shortcuts **inside each command branch** (which already happens) but then **re-compute the lock with the resolved name**. Specifically:

1. For commands that might use shortcuts: move lock acquisition AFTER shortcut resolution within each command branch.
2. The current early-lock pattern works for non-shortcut names. Only "latest"/"previous" bypass the lock.
3. Alternative: resolve "latest"/"previous" to real names BEFORE lock, using `list_local()` or `list_remote()`. Local resolution is cheap (filesystem scan). Remote resolution requires S3.

**Chosen approach**: Move lock acquisition into each command branch, AFTER shortcut resolution. This avoids S3 client creation ordering issues and ensures the lock uses the actual backup name.

## Key Observations

1. `validate_backup_name("latest")` returns `Ok(())` -- "latest" passes all validation checks (no `..`, `/`, `\`, NUL, non-empty).
2. `lock_for_command("upload", Some("latest"))` returns `LockScope::Backup("latest".to_string())` which maps to `/tmp/chbackup.latest.pid` -- NOT the actual backup name.
3. `list_local` and `list_remote` sort by `name.cmp` (lexicographic), NOT by `timestamp`. The doc comment says "chronological if names are date-based" -- relies on naming convention.
4. `create_dir_all` is idempotent -- does not fail if directory exists, enabling silent overwrites.
5. `resolve_backup_name(None)` produces `Utc::now().format("%Y-%m-%dT%H%M%S")` -- second-precision timestamp.
6. `backup::create()` does NOT accept a `resume` parameter. The `--resume` flag is handled in `main.rs` by just logging a message.
7. The design doc (line 919) says `--resume` applies to `create, upload, download, restore`. The implementation defers create resume with a comment.
8. Retention sorts by `timestamp.cmp` (list.rs:752, 1085) while `resolve_backup_shortcut` relies on `name.cmp` ordering. These could diverge for custom-named backups.
9. Doctests for `path_encoding` currently PASS (verified `cargo test --doc path_encoding`). Finding may be stale.
