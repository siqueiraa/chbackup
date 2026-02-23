# Compiler Diagnostics

## Cargo Check Results

**Timestamp:** 2026-02-20
**Command:** `cargo check`
**Result:** Clean build -- 0 errors, 0 warnings

```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.73s
```

## Pre-Existing Issues

None. The codebase compiles cleanly with zero warnings (matches the zero warnings policy in CLAUDE.md).

## Clippy Notes

The `collect_parts()` function already has `#[allow(clippy::too_many_arguments)]` (9 params). Adding a `backup_name` parameter (needed for per-disk backup dir construction) would be the 10th parameter. This is acceptable given the existing pattern but should be considered for potential refactoring (e.g., a struct for collect params) in a follow-up.

## Key Compiler-Visible Observations

1. `disk_path_to_name` (line 131-137 of collect.rs) is currently **unused** -- suppressed with `let _ = &disk_path_to_name;` (line 336). This was "reserved for future use" but is exactly the kind of map we might need. However, the per-disk plan does NOT need this reverse map since we already have `disk_name` and `disk_path` in scope during the walk loop.

2. The `backup_dir` parameter in `collect_parts()` is currently used ONLY for computing `staging_dir` for local disk parts (line 303-307). S3 disk parts skip hardlinking entirely. This makes the change surgical: only the `staging_dir` computation needs to change.

3. All callers of `collect_parts()` are within `backup/mod.rs` (inside a `tokio::spawn` block at line 507) and two tests in `backup/collect.rs`.
