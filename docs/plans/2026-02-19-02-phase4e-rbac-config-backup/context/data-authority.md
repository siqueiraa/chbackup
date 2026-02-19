# Data Authority Analysis

## RBAC Data

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| User definitions | system.users | name, storage, auth_type, auth_params | USE EXISTING - query system.users |
| Role definitions | system.roles | name, storage | USE EXISTING - query system.roles |
| Row policies | system.row_policies | name, storage, short_name, database, table | USE EXISTING - query system.row_policies |
| Settings profiles | system.settings_profiles | name, storage | USE EXISTING - query system.settings_profiles |
| Quotas | system.quotas | name, storage | USE EXISTING - query system.quotas |
| Quota limits | system.quota_limits | quota_name, max_queries, etc. | USE EXISTING - associated with quotas |
| User grants | system.grants | user_name, role_name, access_type, etc. | USE EXISTING - grants for users/roles |
| User role mappings | system.role_grants | user_name, granted_role_name | USE EXISTING - role-to-user mappings |

## Named Collections Data

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Named collection definitions | system.named_collections | name, collection (JSON) | USE EXISTING - query provides all data |
| Named collection DDL | SHOW CREATE NAMED COLLECTION | Full CREATE DDL | USE EXISTING - reconstruct via DDL |

## Config Files Data

| Data Needed | Source | Decision |
|-------------|--------|----------|
| ClickHouse config files | config_dir filesystem | MUST IMPLEMENT - file copy from config_dir to backup |

## RBAC Restore Data

| Data Needed | Source | Decision |
|-------------|--------|----------|
| RBAC file restore target | ClickHouse access_data_path | MUST IMPLEMENT - typically /var/lib/clickhouse/access/ |
| need_rebuild_lists.mark | Filesystem operation | MUST IMPLEMENT - create marker file |
| Stale .list files | Filesystem operation | MUST IMPLEMENT - remove existing .list files |
| Chown access files | Filesystem operation | MUST IMPLEMENT - reuse existing chown pattern from attach.rs |
| Restart command exec | Config restart_command | MUST IMPLEMENT - parse and execute command |

## Analysis Notes

1. **RBAC Backup approach**: The Go tool (clickhouse-backup) queries system tables and writes `.jsonl` files. The alternative approach of using `SHOW CREATE USER/ROLE/etc` DDL statements is simpler and maps better to the existing `functions: Vec<String>` pattern in the manifest. However, the design doc specifies `.jsonl` files in `access/` directory, so we follow that.

2. **Two RBAC restore approaches available**:
   - **File-based (design doc approach)**: Copy `.jsonl` files to `access_data_path`, create `need_rebuild_lists.mark`, restart ClickHouse. This is the Go tool's approach and what the design doc specifies.
   - **SQL-based**: Use `CREATE USER`, `CREATE ROLE`, etc. SQL statements. Simpler but doesn't handle all RBAC edge cases.

   We follow the design doc: file-based restore for RBAC.

3. **Named collections**: SQL-based approach (`CREATE NAMED COLLECTION` DDL) is straightforward. Store DDL strings in `manifest.named_collections` (already a `Vec<String>` field).

4. **Config backup**: Pure filesystem copy. No ClickHouse query needed. Source: `config.clickhouse.config_dir` (default `/etc/clickhouse-server`). Destination in backup: `configs/` directory.

5. **restart_command parsing**: Design doc specifies prefixes `exec:` for shell commands and `sql:` for ClickHouse queries. Multiple commands separated by `;`. All errors are logged and ignored (best-effort).

6. **ClickHouse access_data_path**: Typically `/var/lib/clickhouse/access/` but should be queried or derived from `data_path` config (`{data_path}/access/`).
