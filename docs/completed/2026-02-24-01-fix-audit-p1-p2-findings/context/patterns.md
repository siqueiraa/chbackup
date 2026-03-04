# Pattern Discovery

## Global Pattern Registry

No global patterns directory exists (`docs/patterns/` not found).

## Patterns Discovered from Codebase

### 1. Lock Scope Pattern (lock.rs)

```
lock_for_command(command, backup_name) -> LockScope
```

- Backup-scoped: create, upload, download, restore, create_remote, restore_remote
  - With name: `LockScope::Backup(name)` -> `/tmp/chbackup.{name}.pid`
  - Without name: `LockScope::Global`
- Global: clean, clean_broken, delete
- None: list, tables, default-config, print-config, watch, server

Key observation: `backup_name` comes from CLI BEFORE shortcut resolution.

### 2. Backup Name Resolution Pattern (main.rs)

Two resolution paths:
- `resolve_backup_name(Option<String>)` -- for create/create_remote (generates auto-name if None)
- `backup_name_required(Option<String>, &str)` -- for upload/download/restore (requires name)

After name extraction, shortcuts are resolved:
- `resolve_local_shortcut(name, data_path)` -- scans local backups
- `resolve_remote_shortcut(name, s3)` -- scans remote backups

Both delegate to `resolve_backup_shortcut(name, &[BackupSummary])` which uses `backups.last()` from a list sorted by `a.name.cmp(&b.name)` (lexicographic).

### 3. Backup Directory Creation Pattern (backup/mod.rs)

```rust
std::fs::create_dir_all(&backup_dir)  // line 286
```

Uses `create_dir_all` which succeeds silently if directory already exists. No pre-existence check.

### 4. CLI Flag Mutual Exclusion Pattern

Clap does not enforce mutual exclusion between `--schema` and `--data-only` on the restore command. Both are `#[arg(long)]` with `bool` type, no `conflicts_with` attribute.

### 5. Restore Schema/Data Flow (restore/mod.rs)

```
if schema_only { ... return Ok(()); }   // line 243-320: exits early, skips data
if !data_only { ... }                    // line 219: guards schema creation
```

When both `schema_only=true` and `data_only=true`:
- Phase 0 DROP: skipped (guarded by `rm && !data_only`)
- Phase 1 CREATE DBs: skipped (guarded by `!data_only`)
- Phase 2 CREATE tables: called with `data_only=true` (internal guard skips creation)
- Then `schema_only` branch runs -> returns Ok(()) without attaching data
- Net effect: no schema created, no data attached = silent no-op

### 6. Backup Naming Precision Pattern

Auto-generated names use `Utc::now().format("%Y-%m-%dT%H%M%S")` which has **second precision**. Two backups created within the same second get the same name.

### 7. Doctest Import Pattern

Doctests in `path_encoding.rs` use `use chbackup::path_encoding::encode_path_component;` which requires the module to be publicly exported in `lib.rs`. Verified: `pub mod path_encoding;` exists in `lib.rs` line 12. Doctests currently PASS (verified via `cargo test --doc path_encoding`).

Note: The audit finding about doctests failing with `unresolved import chbackup::path_encoding` may have been observed in a different environment or has since been fixed. Current doctests pass.
