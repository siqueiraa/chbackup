# Data Authority Analysis

## MISSING-2: ListResponse.required (Incremental Base Backup Name)

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Incremental base backup name | PartInfo | `source: String` with value `"carried:{base_name}"` | USE EXISTING |

**Analysis:** The `PartInfo.source` field already encodes the incremental base backup name. When a backup is incremental, its parts have `source = "carried:base_backup_name"`. The extraction pattern already exists in `collect_incremental_bases()` at `src/list.rs:959`:
```rust
if let Some(base_name) = part.source.strip_prefix("carried:") {
    bases.insert(base_name.to_string());
}
```
We reuse this exact pattern to extract base names during `parse_backup_summary()` and `list_remote()`.

## MISSING-4: ListResponse.object_disk_size (S3 Object Disk Size)

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Total S3 object disk size | PartInfo.s3_objects | `Option<Vec<S3ObjectInfo>>` with each having `size: u64` | USE EXISTING |

**Analysis:** The `S3ObjectInfo.size` field already holds the size of each S3 object disk file. The data is available in the manifest during both local and remote backup summary construction. We just need to sum `s3_objects[].size` across all parts in all tables. Similar iteration already exists in `collect_keys_from_manifest()` at `src/list.rs:780`:
```rust
if let Some(ref s3_objects) = part.s3_objects {
    for s3_obj in s3_objects {
        // ... access s3_obj fields ...
    }
}
```

## BUG-1: post_actions Command Dispatch

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Backup name from command | ActionRequest.command | Already split at routes.rs:230 into `parts` | USE EXISTING |
| Command function signatures | Existing endpoint handlers | All wired in routes.rs:342-930 | USE EXISTING - follow pattern |

**Analysis:** All the actual command functions (`backup::create`, `upload::upload`, `download::download`, `restore::restore`, `list::delete_local`, `list::delete_remote`, `list::clean_broken_local`, `list::clean_broken_remote`) are already called from their respective endpoint handlers. The post_actions stub just needs to extract the backup name (second token) and dispatch to the same function calls with default parameters.

## MISSING-1: List Endpoint Pagination

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Pagination pattern | TablesParams (routes.rs:88) | `offset: Option<usize>`, `limit: Option<usize>` | USE EXISTING - copy pattern |
| Format support | ListFormat enum (list.rs:30) | `enum ListFormat { Default, Json, Yaml, Csv, Tsv }` | USE EXISTING |

**Analysis:** The pagination pattern already exists in the tables endpoint. We copy it exactly. The ListFormat enum and `format_list_output()` function already exist in list.rs. We just need to add `offset`, `limit`, `format` fields to `ListParams` and apply the same skip/take + X-Total-Count pattern.

## MISSING-3: SIGTERM Handler

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Signal handler pattern | SIGHUP handler (mod.rs:216) | Signal registration + channel send | USE EXISTING - follow pattern |
| Shutdown logic | ctrl_c handler (mod.rs:298) | Integration table cleanup + watch shutdown | USE EXISTING - merge into SIGTERM |

**Analysis:** No new data computation needed. The SIGTERM handler follows the existing SIGHUP/SIGQUIT pattern for registration, and performs the same cleanup as the ctrl_c handler. Design doc 11.5 says SIGTERM in server mode = same as SIGINT.

## Summary

| Decision | Count | Details |
|----------|-------|---------|
| USE EXISTING | 6 | All data sources already exist in the codebase |
| MUST IMPLEMENT | 0 | No new data computation needed -- all work is wiring existing data through |
