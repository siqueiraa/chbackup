# Pattern Discovery

## Global Pattern Registry

No global `docs/patterns/` directory exists. Patterns discovered locally from existing code.

## Pattern 1: Parallel Pipeline with Flat Semaphore

**Used by:** upload, download, restore, backup (freeze)

**Structure:**
1. Flatten all work items across all tables into single `Vec<WorkItem>`
2. Create shared `Arc<Semaphore>` with `effective_*_concurrency(config)`
3. `tokio::spawn` each work item with semaphore acquire
4. `futures::future::try_join_all` for fail-fast error propagation
5. Sequential aggregation of results after all tasks complete

**Reference implementation:** `src/upload/mod.rs` lines 358-584

**Resume integration point:** Between step 3 (semaphore acquire) and the actual work, check if the part is already completed according to the state file. If yes, skip.

## Pattern 2: Config-Driven Feature Gating

**Used by:** all commands

**Structure:**
- Config field (e.g., `use_resumable_state: bool`) controls behavior
- CLI flag (e.g., `--resume`) triggers resume path
- Both must be true to enable feature

**Reference:** `src/config.rs` line 91 (`use_resumable_state`), `src/cli.rs` lines 73-75 (`--resume`)

## Pattern 3: Buffered Upload/Download

**Used by:** upload, download

**Structure:**
- Compress entire part to `Vec<u8>` via `spawn_blocking`
- Upload via `put_object` (single) or multipart
- Download to `Vec<u8>`, decompress via `spawn_blocking`

**Resume integration:** After compress+upload success, record part key in state file. On resume, skip parts already in state file.

## Pattern 4: Error Context Chain

**Used by:** all modules

**Structure:**
```rust
.with_context(|| format!("Failed to X for part {} in table {}", part_name, table_key))?
```

All errors use `anyhow::Result` with `.context()` for stack-like error messages.

## Pattern 5: State Degradation (design 16.1)

**New for Phase 2d.**

**Structure:**
```rust
match save_state_file(&state_path, &state) {
    Ok(_) => {},
    Err(e) => {
        warn!("Failed to write resumable state: {}. Operation continues but won't be resumable.", e);
    }
}
```

State file write failures are non-fatal warnings. This applies to ALL state file writes.

## Pattern 6: Manifest Atomicity

**New for Phase 2d.**

**Structure:**
1. Upload manifest to `{backup_name}/metadata.json.tmp`
2. CopyObject from `.tmp` to final key `metadata.json`
3. DeleteObject `.tmp`
4. If crash between steps 1 and 2: backup is "broken" (cleaned by `clean_broken`)

**Current code:** Upload goes directly to final key (line 634 in `src/upload/mod.rs`). Must be changed.

## Pattern 7: Broken Backup Detection

**Partially exists in `src/list.rs`:**
- `BackupSummary.is_broken: bool` field already defined
- `list_local()` already checks for missing/corrupt `metadata.json` and marks as broken
- `list_remote()` already handles parse errors and marks as broken

**New for Phase 2d:** Add `[BROKEN]` display marker and `clean_broken` command implementation.

## Pattern 8: Disk Filtering

**Partially exists:**
- `skip_disks: Vec<String>` and `skip_disk_types: Vec<String>` already in config
- `skip_tables` and `skip_table_engines` patterns used in `backup/mod.rs`
- `is_excluded()` and `is_engine_excluded()` helpers exist in `table_filter.rs`

**New for Phase 2d:** Add `is_disk_excluded()` filtering during shadow walk.

## Pattern 9: Partition-Level FREEZE

**Design reference:** Section 3.4

**Structure:**
```
If --partitions is set:
  for each partition in partition_list:
    ALTER TABLE `db`.`table` FREEZE PARTITION 'X' WITH NAME '{freeze_name}'
  Merge shadow results
Default:
  ALTER TABLE `db`.`table` FREEZE WITH NAME '{freeze_name}'
```

**Current code:** Only whole-table FREEZE implemented. `--partitions` flag exists in CLI but logs warning "not yet implemented".
