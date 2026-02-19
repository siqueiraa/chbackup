# Redundancy Analysis

## New Public Components Proposed

| Proposed | Existing Match | Decision | Task/Acceptance | Justification |
|----------|---------------|----------|-----------------|---------------|
| `backup_rbac()` fn in backup/mod.rs or new rbac.rs | None | COEXIST | N/A | New functionality, no existing equivalent |
| `backup_configs()` fn | None | COEXIST | N/A | New functionality |
| `backup_named_collections()` fn | None | COEXIST | N/A | New functionality |
| `restore_rbac()` fn in restore/schema.rs or new rbac.rs | None | COEXIST | N/A | New functionality |
| `restore_configs()` fn | None | COEXIST | N/A | New functionality |
| `restore_named_collections()` fn | `create_functions()` in restore/schema.rs | EXTEND | N/A | Follow same pattern, add as sibling function |
| `execute_restart_command()` fn | None | COEXIST | N/A | New functionality for post-restore restart |
| `query_rbac_users()` ChClient method | None | COEXIST | N/A | New system table query |
| `query_rbac_roles()` ChClient method | None | COEXIST | N/A | New system table query |
| `query_rbac_row_policies()` ChClient method | None | COEXIST | N/A | New system table query |
| `query_rbac_settings_profiles()` ChClient method | None | COEXIST | N/A | New system table query |
| `query_rbac_quotas()` ChClient method | None | COEXIST | N/A | New system table query |
| `query_named_collections()` ChClient method | None | COEXIST | N/A | New system table query |

## Analysis

All proposed new public components are genuinely new functionality with no existing equivalents. The closest pattern is `create_functions()` which serves as the template for `restore_named_collections()`.

No REPLACE decisions needed. No cleanup deadlines needed since all new components serve unique purposes.

## Existing Code Reuse Opportunities

1. **`create_functions()` pattern** (restore/schema.rs:715-755): Template for named collections restore -- sequential DDL execution with non-fatal failures and ON CLUSTER support.

2. **`detect_clickhouse_ownership()` + chown pattern** (restore/attach.rs): Reuse for chowning RBAC access files.

3. **`spawn_blocking` for filesystem I/O** (backup/collect.rs): Reuse for config file copy and RBAC file operations.

4. **`url_encode_component()`** (upload/mod.rs): Reuse for S3 key construction of access/ and configs/ files.

5. **Upload/download part patterns**: The existing `put_object` / `get_object` from S3Client can be reused for individual RBAC/config files (they're small, no need for multipart or tar+lz4).
