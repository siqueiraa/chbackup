# Redundancy Analysis

## Proposed New Public Components

| Proposed | Existing Match | Decision | Justification |
|----------|----------------|----------|---------------|
| `retention_local(data_path, keep) -> Result<usize>` | `clean_broken_local(data_path) -> Result<usize>` | COEXIST | Different semantics: clean_broken deletes by broken status; retention deletes by age/count. Both needed permanently. No cleanup deadline needed -- they serve different purposes. |
| `retention_remote(s3, keep) -> Result<usize>` | `clean_broken_remote(s3) -> Result<usize>` | COEXIST | Different semantics: clean_broken deletes by broken status; retention deletes oldest exceeding count with GC safety. Both needed permanently. |
| `gc_collect_referenced_keys(s3, exclude) -> Result<HashSet<String>>` | (none) | N/A | No existing equivalent. New computation. |
| `gc_delete_backup(s3, name, referenced_keys) -> Result<()>` | `delete_remote(s3, name) -> Result<()>` | COEXIST | `delete_remote` does naive delete-all-keys. `gc_delete_backup` is GC-safe: only deletes unreferenced keys, then deletes manifest last. Both needed: `delete_remote` for explicit user delete (user knows what they are doing), `gc_delete_backup` for automated retention (must protect shared keys). |
| `clean_shadow(ch, data_path, name) -> Result<usize>` | (none) | N/A | No existing shadow cleanup function. |
| `effective_retention_local(config) -> i32` | (none) | N/A | No existing config resolution helper. Simple helper function. |
| `effective_retention_remote(config) -> i32` | (none) | N/A | No existing config resolution helper. Simple helper function. |

## Analysis Details

### retention_local vs clean_broken_local (COEXIST)

These serve fundamentally different purposes:
- `clean_broken_local`: Deletes backups with missing/corrupt metadata.json. Safety measure.
- `retention_local`: Deletes oldest valid backups exceeding a count threshold. Lifecycle management.

Both are needed permanently. The design doc treats them as separate operations (8.1/8.3 vs 8.4).

### gc_delete_backup vs delete_remote (COEXIST)

- `delete_remote`: Deletes ALL S3 keys under the backup prefix. Used for explicit `chbackup delete remote <name>` command.
- `gc_delete_backup`: Implements the GC algorithm from design 8.2 -- only deletes keys NOT referenced by any surviving backup. Required for retention because incremental backups share S3 keys via `carried:` parts.

Both are needed permanently:
- `delete_remote` for user-initiated deletes (user takes responsibility for breaking incremental chains)
- `gc_delete_backup` for automated retention (must protect surviving backups' data)

### No REPLACE Decisions

No existing functions are being replaced. All new functions serve distinct purposes.
