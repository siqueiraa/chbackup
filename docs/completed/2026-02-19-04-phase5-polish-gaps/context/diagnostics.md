# Diagnostics

## Compiler State (cargo check)

**Result: CLEAN -- 0 errors, 0 warnings**

```
$ cargo check
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.69s
```

## Clippy Analysis

**Result: CLEAN -- 0 errors, 0 warnings**

```
$ cargo clippy
    Checking chbackup v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.95s
```

## Pre-existing Issues

None. The codebase compiles cleanly with zero warnings.

## Stub/TODO Inventory (Relevant to Plan)

| Location | Current State | Plan Item |
|---|---|---|
| `src/server/routes.rs:1187` | `restart_stub()` returns 501 | Item 2: Implement restart endpoint |
| `src/server/routes.rs:1192` | `tables_stub()` returns 501 | Item 1: Implement tables endpoint |
| `src/server/routes.rs:282` | `metadata_size: 0` hardcoded | Item 7: Expose from manifest |
| `src/server/routes.rs:283` | `rbac_size: 0` hardcoded | Item 7: Compute from manifest |
| `src/server/routes.rs:284` | `config_size: 0` hardcoded | Item 7: Compute from manifest |
| `src/main.rs:135-136` | `skip_projections` warns "not yet implemented" | Item 3: Implement filtering |
| `src/main.rs:198-199` | `hardlink_exists_files` warns "not yet implemented" | Item 4: Implement dedup |
| `src/main.rs:280-281` | `skip_projections` warns "not yet implemented" (create_remote) | Item 3: Also in create_remote |
| `src/server/routes.rs:480-481` | `hardlink_exists_files` warns "not yet implemented" in API route | Item 4: Also in API |
| `src/config.rs:47` | `disable_progress_bar: bool` exists but unused | Item 5: No progress bar impl |
| No `process::exit` anywhere | All exits via `Ok(())` or `anyhow::bail!` | Item 6: No structured exit codes |
