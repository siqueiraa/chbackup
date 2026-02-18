# Data Authority Analysis

## Data Requirements

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Backup duration (create/upload/download/restore) | `std::time::Instant` timing | Measured at call site | MUST IMPLEMENT - wrap operation with timer |
| Backup compressed size | `BackupManifest` | `compressed_size: u64` (manifest.rs:44) | USE EXISTING |
| Last success timestamp | `chrono::Utc::now()` | Available at finish time | MUST IMPLEMENT - record timestamp on success |
| Parts uploaded count | Upload result tracking | Parts processed in upload loop (upload/mod.rs:298) | MUST IMPLEMENT - count in upload loop, or count non-carried parts from manifest |
| Parts skipped (incremental) | `DiffResult` / `PartInfo.source` | `DiffResult.carried: usize` (diff.rs:16), or parts with `source.starts_with("carried:")` | USE EXISTING - count carried parts from manifest |
| Errors per operation | Operation result | `Err(e)` in match arm | MUST IMPLEMENT - increment counter on error |
| Number backups local | `list::list_local()` | Returns `Vec<BackupSummary>` (list.rs:81) | USE EXISTING |
| Number backups remote | `list::list_remote()` | Returns `Vec<BackupSummary>` (list.rs:125) | USE EXISTING |
| In progress flag | `AppState.current_op` | `Arc<Mutex<Option<RunningOp>>>` | USE EXISTING - check if Some |
| Watch state | Watch mode (Phase 3d) | NOT YET IMPLEMENTED | DEFER - set to 0/"idle" until Phase 3d |
| Watch last full timestamp | Watch mode (Phase 3d) | NOT YET IMPLEMENTED | DEFER - set to 0 until Phase 3d |
| Watch consecutive errors | Watch mode (Phase 3d) | NOT YET IMPLEMENTED | DEFER - set to 0 until Phase 3d |

## Analysis Notes

1. **Backup duration**: Not available from existing data structures. Must wrap each operation call with `Instant::now()` and `elapsed()` in the spawned task. This is straightforward since all operations are already wrapped in a match result block.

2. **Parts uploaded vs skipped**: Two approaches possible:
   - Count during upload loop (requires modifying upload module internal code)
   - Count from manifest after operation: parts with `source == "uploaded"` vs `source.starts_with("carried:")`
   - **Decision**: Count from manifest after create completes. The manifest already records part sources. For upload, count non-carried parts in the upload loop. Simpler: pass metrics into spawned task and increment in the match arms at the aggregate level (total parts = manifest table parts count).

3. **Backup counts**: `list_local()` and `list_remote()` are synchronous/async functions that return `Vec<BackupSummary>`. They are called on each `/metrics` scrape to populate gauges. `list_local()` is a blocking directory scan so must be called via `spawn_blocking`.

4. **Watch mode metrics**: Watch mode is Phase 3d (not yet implemented). The watch metrics (`watch_state`, `watch_last_full_timestamp`, `watch_consecutive_errors`) are registered but initialized to defaults (0/idle). They will be updated when watch mode is implemented. This follows the existing stub pattern.

5. **In-progress gauge**: Can be computed from `AppState.current_op` (is it Some or None). Set to 1 at `try_start_op()`, set to 0 at `finish_op()`/`fail_op()`/`kill_current()`. Alternatively, just check the mutex value on scrape.

## Decisions Summary

- **USE EXISTING**: 4 items (compressed_size, carried parts, backup counts local, backup counts remote)
- **MUST IMPLEMENT**: 4 items (duration timing, success timestamp, error counting, in-progress gauge management)
- **DEFER**: 3 items (watch_state, watch_last_full_timestamp, watch_consecutive_errors -- Phase 3d)
