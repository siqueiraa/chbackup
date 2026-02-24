# Type Verification Table

## Verified Types

| Variable/Field | Assumed Type | Actual Type | Verification Source |
|---|---|---|---|
| `WatchConfig` | struct with `watch_interval: String`, `full_interval: String`, `enabled: bool` | `pub struct WatchConfig { enabled: bool, watch_interval: String, full_interval: String, name_template: String, tables: Option<String>, ... }` | config.rs:404-436 (direct read) |
| `Config::validate` | `fn(&self) -> Result<()>` | `pub fn validate(&self) -> Result<()>` | LSP hover @ config.rs:1380 |
| `parse_duration_secs` | `fn(&str) -> Result<u64>` | `pub fn parse_duration_secs(s: &str) -> Result<u64>` | LSP hover @ config.rs:1542 |
| `BackupSummary` | struct with `name: String`, `timestamp: Option<DateTime<Utc>>`, `is_broken: bool` | Confirmed: `pub struct BackupSummary { name: String, timestamp: Option<DateTime<Utc>>, size: u64, compressed_size: u64, table_count: usize, metadata_size: u64, rbac_size: u64, config_size: u64, object_disk_size: u64, ... }` | list.rs:46-66 (grep) |
| `ResumeDecision` | enum: FullNow, IncrNow{diff_from}, SleepThen{remaining, backup_type} | Confirmed exactly | watch/mod.rs:91-101 (direct read) |
| `watch_start` handler | `fn(State<AppState>) -> Result<Json<WatchActionResponse>, ...>` | `pub async fn watch_start(State(mut state): State<AppState>) -> Result<Json<WatchActionResponse>, (StatusCode, Json<ErrorResponse>)>` | LSP hover @ routes.rs:1601 |
| `spawn_watch_from_state` | `fn(&mut AppState, PathBuf, HashMap<String, String>)` | `pub async fn spawn_watch_from_state(state: &mut AppState, config_path: PathBuf, macros: HashMap<String, String>)` | LSP hover @ server/mod.rs:381 |
| `start_server` | `fn(Arc<Config>, ChClient, S3Client, bool, PathBuf) -> Result<()>` | `pub async fn start_server(config: Arc<Config>, ch: ChClient, s3: S3Client, watch: bool, config_path: PathBuf) -> Result<()>` | LSP hover @ server/mod.rs:142 |
| `WatchActionResponse` | struct with `status: String` | `pub struct WatchActionResponse { pub status: String }` | routes.rs:1679-1681 (grep) |
| `ErrorResponse` | struct with `error: String` | `pub struct ErrorResponse { pub error: String }` | routes.rs:118-120 (grep) |
| `rewrite_distributed_engine` | `fn(ddl, src_db, src_table, dst_db, dst_table) -> String` | `fn rewrite_distributed_engine(ddl: &str, src_db: &str, src_table: &str, dst_db: &str, dst_table: &str) -> String` (private) | remap.rs:599-604 (grep) |
| `strip_quotes` | `fn(&str) -> &str` | `fn strip_quotes(s: &str) -> &str` (private) | remap.rs:731-738 (grep) |
| `resolve_name_template` | `fn(&str, &str, DateTime<Utc>, &HashMap<String,String>) -> String` | `pub fn resolve_name_template(template: &str, backup_type: &str, now: DateTime<Utc>, macros: &HashMap<String, String>) -> String` | watch/mod.rs:34-38 (direct read) |
| `resolve_template_prefix` | `fn(&str) -> String` | `pub fn resolve_template_prefix(name_template: &str) -> String` | watch/mod.rs:107 (direct read) |
| `Command::Server` | enum variant with `watch: bool` | `Server { #[arg(long)] watch: bool }` | cli.rs:344-348 (direct read) |
| `Command::Watch` | enum variant with 4 optional fields | `Watch { watch_interval: Option<String>, full_interval: Option<String>, name_template: Option<String>, tables: Option<String> }` | cli.rs:325-341 (direct read) |

## New Types to Define

| Type | Location | Pattern Reference |
|---|---|---|
| `WatchStartRequest` | src/server/routes.rs | Follow `CreateRequest` pattern at routes.rs:743 -- `#[derive(Debug, Deserialize, Default)]` with `Option<String>` fields |

## Anti-Pattern Checks

- No `.as_str()` on enum types (no enums in this plan)
- No implicit String->Enum conversions
- `parse_duration_secs` takes `&str` and returns `Result<u64>` -- callers must handle errors
- `WatchConfig.watch_interval` and `full_interval` are both `String` type (duration strings like "1h", "24h")
