# Type Verification

## Existing Types (Verified in Source)

| Variable/Field | Type | Location | Verification |
|---|---|---|---|
| `BackupManifest.functions` | `Vec<String>` | src/manifest.rs:73 | Read file directly |
| `BackupManifest.named_collections` | `Vec<String>` | src/manifest.rs:77 | Read file directly |
| `BackupManifest.rbac` | `Option<RbacInfo>` | src/manifest.rs:81 | Read file directly |
| `RbacInfo.path` | `String` | src/manifest.rs:189 | Read file directly |
| `Config.clickhouse.rbac_backup_always` | `bool` | src/config.rs:198 | Read file directly |
| `Config.clickhouse.config_backup_always` | `bool` | src/config.rs:202 | Read file directly |
| `Config.clickhouse.named_collections_backup_always` | `bool` | src/config.rs:206 | Read file directly |
| `Config.clickhouse.rbac_resolve_conflicts` | `String` | src/config.rs:210 | Read file directly |
| `Config.clickhouse.restart_command` | `String` | src/config.rs:190 | Read file directly |
| `Config.clickhouse.config_dir` | `String` | src/config.rs:115 | Read file directly |
| `Config.clickhouse.data_path` | `String` | src/config.rs:112 | Read file directly |
| `ChClient` | struct (Clone) | src/clickhouse/client.rs:14 | Read file directly |
| `TableRow.database` | `String` | src/clickhouse/client.rs:25 | Read file directly |
| `TableRow.name` | `String` | src/clickhouse/client.rs:26 | Read file directly |
| `TableRow.engine` | `String` | src/clickhouse/client.rs:27 | Read file directly |

## CLI Flag Types (Verified)

| Flag | Type | Location |
|---|---|---|
| `Create.rbac` | `bool` | src/cli.rs:59 |
| `Create.configs` | `bool` | src/cli.rs:63 |
| `Create.named_collections` | `bool` | src/cli.rs:67 |
| `Restore.rbac` | `bool` | src/cli.rs:149 |
| `Restore.configs` | `bool` | src/cli.rs:153 |
| `Restore.named_collections` | `bool` | src/cli.rs:157 |
| `CreateRemote.rbac` | `bool` | src/cli.rs:184 |
| `CreateRemote.configs` | `bool` | src/cli.rs:188 |
| `CreateRemote.named_collections` | `bool` | src/cli.rs:192 |
| `RestoreRemote.rbac` | `bool` | src/cli.rs:231 |
| `RestoreRemote.configs` | `bool` | src/cli.rs:239 |

## New Types to Create

| Type | Purpose | Fields |
|---|---|---|
| `RbacObject` | Deserialized RBAC entity from system tables | `name: String`, `storage: String`, `auth_type: String` (varies by entity type) |
| N/A for named collections | Use `Vec<String>` of CREATE DDL | Already in manifest as `Vec<String>` |

## ChClient Methods to Add

| Method | Signature | Purpose |
|---|---|---|
| `query_rbac_users` | `async fn(&self) -> Result<Vec<RbacUserRow>>` | Query system.users |
| `query_rbac_roles` | `async fn(&self) -> Result<Vec<RbacRoleRow>>` | Query system.roles |
| `query_rbac_row_policies` | `async fn(&self) -> Result<Vec<RbacRowPolicyRow>>` | Query system.row_policies |
| `query_rbac_settings_profiles` | `async fn(&self) -> Result<Vec<RbacSettingsProfileRow>>` | Query system.settings_profiles |
| `query_rbac_quotas` | `async fn(&self) -> Result<Vec<RbacQuotaRow>>` | Query system.quotas |
| `query_named_collections` | `async fn(&self) -> Result<Vec<NamedCollectionRow>>` | Query system.named_collections |
| `get_access_data_path` | `async fn(&self) -> Result<String>` | Query SELECT path FROM system.disks WHERE name='default' + /access/ |

## Existing Functions to Modify

| Function | File | Change |
|---|---|---|
| `backup::create()` | src/backup/mod.rs | Add rbac/configs/named_collections params, populate manifest fields |
| `restore::restore()` | src/restore/mod.rs | Add Phase 4 RBAC/config/named-collections restore |
| `main.rs` Create handler | src/main.rs:118-167 | Pass rbac/configs/named_collections through |
| `main.rs` Restore handler | src/main.rs:219-275 | Pass rbac/configs/named_collections through |
| `main.rs` CreateRemote handler | src/main.rs:277-338 | Pass rbac/configs/named_collections through |
| `main.rs` RestoreRemote handler | src/main.rs:340-395 | Pass rbac/configs/named_collections through |
| `upload::upload()` | src/upload/mod.rs | Upload access/ and configs/ directories |
| `download::download()` | src/download/mod.rs | Download access/ and configs/ directories |
