# Diagnostics Report

## Compiler State

**Timestamp:** 2026-02-18
**Command:** `cargo check`
**Result:** Clean compilation - no errors, no warnings

```
Checking chbackup v0.1.0 (/Users/rafael.siqueira/dev/personal/chbackup)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.06s
```

## Summary

| Category | Count |
|----------|-------|
| Errors | 0 |
| Warnings | 0 |
| Lint issues | 0 |

## Baseline Confidence

The codebase compiles cleanly. All Phase 0-3e work is complete and merged to main/master. No pre-existing issues that could interfere with Phase 4a work.

## Existing Stub/Placeholder Code

The following stubs exist in the codebase that Phase 4a will replace:

### 1. `main.rs:234-238` - `--as` flag warning
```rust
if rename_as.is_some() {
    warn!("--as flag is not yet implemented, ignoring");
}
```

### 2. `main.rs:237-239` - `-m` flag warning
```rust
if database_mapping.is_some() {
    warn!("--database-mapping flag is not yet implemented, ignoring");
}
```

### 3. `main.rs:340-342` - `restore_remote` stub
```rust
Command::RestoreRemote { backup_name, .. } => {
    info!(backup_name = ?backup_name, "restore_remote: not implemented in Phase 1");
}
```

### 4. `src/server/routes.rs:547-549` - Server route `database_mapping` warning
```rust
if req.database_mapping.is_some() {
    warn!("database_mapping is not yet implemented (Phase 4a), ignoring");
}
```

### 5. `src/server/routes.rs:821-825` - `RestoreRemoteRequest` missing remap fields
```rust
pub struct RestoreRemoteRequest {
    pub tables: Option<String>,
    pub schema: Option<bool>,
    pub data_only: Option<bool>,
    // Missing: rename_as, database_mapping, rm
}
```

## CLI Flags Already Defined

The `--as` and `-m` flags are already defined in `cli.rs` for both `Restore` and `RestoreRemote` variants:
- `Cli::Restore::rename_as` at line 121 (`--as`)
- `Cli::Restore::database_mapping` at line 125 (`-m`)
- `Cli::RestoreRemote::rename_as` at line 219 (`--as`)
- `Cli::RestoreRemote::database_mapping` at line 223 (`-m`)

No CLI changes needed -- only implementation wiring.

## Config Params Already Defined

The following config params relevant to DDL rewriting already exist in `src/config.rs`:
- `clickhouse.default_replica_path`: `/clickhouse/tables/{shard}/{database}/{table}` (line 228-229)
- `clickhouse.default_replica_name`: `{replica}` (line 231-232)
- `clickhouse.restore_distributed_cluster`: empty string (line 162)
- `clickhouse.restore_schema_on_cluster`: empty string (line 160)
