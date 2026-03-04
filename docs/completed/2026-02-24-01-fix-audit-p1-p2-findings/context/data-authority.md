# Data Authority Analysis

This plan fixes correctness bugs. No new tracking fields, accumulators, or calculations are being added.

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Backup name for lock | CLI arg + shortcut resolution | Already in main.rs flow | USE EXISTING - reorder resolution |
| Backup directory existence | std::fs | PathBuf::exists() | USE EXISTING |
| Backup list for shortcut | list_local / list_remote | Vec<BackupSummary> sorted by name | USE EXISTING - change sort key |
| Schema/data flags | CLI args via clap | bool fields | USE EXISTING - add conflicts_with |
| Create resume state | backup::create | None (not implemented) | MUST IMPLEMENT - design doc lists it, but deferred per code comment |

## Analysis Notes

- All fixes modify existing data flows, not adding new ones.
- The `create --resume` finding is a documentation/API mismatch: the design doc says `--resume` applies to create, but the implementation only logs a message. The fix is documentation-only OR removing the flag from create.
- The `latest`/`previous` sort order issue: `list_local` and `list_remote` sort by `name.cmp` (lexicographic), while retention sorts by `timestamp.cmp`. The `resolve_backup_shortcut` function relies on the name sort. For auto-generated names (`YYYY-MM-DDTHHMMSS`), lexicographic == chronological. For custom names, lexicographic order may not match actual creation time.
