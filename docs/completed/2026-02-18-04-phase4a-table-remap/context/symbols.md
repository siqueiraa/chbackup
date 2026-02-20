# Type Verification Table

## Types Used in This Plan

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `restore()` return | `Result<()>` | `Result<()>` | `src/restore/mod.rs:57` |
| `restore()` `table_pattern` | `Option<&str>` | `Option<&str>` | `src/restore/mod.rs:53` |
| `restore()` `schema_only` | `bool` | `bool` | `src/restore/mod.rs:54` |
| `restore()` `data_only` | `bool` | `bool` | `src/restore/mod.rs:55` |
| `restore()` `resume` | `bool` | `bool` | `src/restore/mod.rs:56` |
| `download()` return | `Result<PathBuf>` | `Result<PathBuf>` | `src/download/mod.rs:141` |
| `BackupManifest.tables` | `HashMap<String, TableManifest>` | `HashMap<String, TableManifest>` | `src/manifest.rs:65` |
| `BackupManifest.databases` | `Vec<DatabaseInfo>` | `Vec<DatabaseInfo>` | `src/manifest.rs:69` |
| `TableManifest.ddl` | `String` | `String` | `src/manifest.rs:88` |
| `TableManifest.uuid` | `Option<String>` | `Option<String>` | `src/manifest.rs:92` |
| `TableManifest.engine` | `String` | `String` | `src/manifest.rs:96` |
| `TableManifest.parts` | `HashMap<String, Vec<PartInfo>>` | `HashMap<String, Vec<PartInfo>>` | `src/manifest.rs:104` |
| `TableManifest.metadata_only` | `bool` | `bool` | `src/manifest.rs:112` |
| `DatabaseInfo.name` | `String` | `String` | `src/manifest.rs:165` |
| `DatabaseInfo.ddl` | `String` | `String` | `src/manifest.rs:168` |
| `OwnedAttachParams.db` | `String` | `String` | `src/restore/attach.rs:63` |
| `OwnedAttachParams.table` | `String` | `String` | `src/restore/attach.rs:65` |
| `OwnedAttachParams.engine` | `String` | `String` | `src/restore/attach.rs:77` |
| `OwnedAttachParams.table_uuid` | `Option<String>` | `Option<String>` | `src/restore/attach.rs:89` |
| `ChClient.execute_ddl()` | `async fn(&str) -> Result<()>` | `pub async fn execute_ddl(&self, ddl: &str) -> Result<()>` | `src/clickhouse/client.rs:441` |
| `ChClient.database_exists()` | `async fn(&str) -> Result<bool>` | `pub async fn database_exists(&self, db: &str) -> Result<bool>` | `src/clickhouse/client.rs:482` |
| `ChClient.table_exists()` | `async fn(&str, &str) -> Result<bool>` | `pub async fn table_exists(&self, db: &str, table: &str) -> Result<bool>` | `src/clickhouse/client.rs:504` |
| `ChClient.list_tables()` | `async fn() -> Result<Vec<TableRow>>` | `pub async fn list_tables(&self) -> Result<Vec<TableRow>>` | `src/clickhouse/client.rs:250` |
| `TableRow.create_table_query` | `String` | `String` | `src/clickhouse/client.rs:29` |
| `TableRow.uuid` | `String` | `String` | `src/clickhouse/client.rs:30` |
| `TableRow.database` | `String` | `String` | `src/clickhouse/client.rs:25` |
| `TableRow.name` | `String` | `String` | `src/clickhouse/client.rs:26` |
| `TableRow.engine` | `String` | `String` | `src/clickhouse/client.rs:27` |
| `Cli::Restore::rename_as` | `Option<String>` | `Option<String>` | `src/cli.rs:121` (clap arg `--as`) |
| `Cli::Restore::database_mapping` | `Option<String>` | `Option<String>` | `src/cli.rs:125` (clap arg `-m`) |
| `Cli::RestoreRemote::rename_as` | `Option<String>` | `Option<String>` | `src/cli.rs:219` |
| `Cli::RestoreRemote::database_mapping` | `Option<String>` | `Option<String>` | `src/cli.rs:223` |
| `RestoreRemoteRequest.tables` | `Option<String>` | `Option<String>` | `src/server/routes.rs:822` |
| `RestoreRemoteRequest.schema` | `Option<bool>` | `Option<bool>` | `src/server/routes.rs:823` |
| `RestoreRemoteRequest.data_only` | `Option<bool>` | `Option<bool>` | `src/server/routes.rs:824` |

## Key Anti-Pattern Checks

- No `.as_str()` on types that are already `String` -- all DDL fields are `String`, not enums
- No implicit type conversions needed -- remap works with string manipulation on DDL strings
- `database_mapping` is parsed from CLI as a single `Option<String>` (comma-separated format like `prod:staging,logs:logs_copy`), needs parsing into `HashMap<String, String>` at the call site
