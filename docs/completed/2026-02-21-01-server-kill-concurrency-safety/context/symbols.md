# Type Verification

## Types Used in This Plan

| Variable/Field | Assumed Type | Actual Type | Verification |
|---|---|---|---|
| `AppState.current_op` | `Arc<Mutex<Option<RunningOp>>>` | `Arc<Mutex<Option<RunningOp>>>` | state.rs:70 |
| `AppState.op_semaphore` | `Arc<Semaphore>` | `Arc<Semaphore>` | state.rs:71 |
| `AppState.config` | `Arc<ArcSwap<Config>>` | `Arc<ArcSwap<Config>>` | state.rs:66 |
| `AppState.ch` | `Arc<ArcSwap<ChClient>>` | `Arc<ArcSwap<ChClient>>` | state.rs:67 |
| `AppState.s3` | `Arc<ArcSwap<S3Client>>` | `Arc<ArcSwap<S3Client>>` | state.rs:68 |
| `AppState.action_log` | `Arc<Mutex<ActionLog>>` | `Arc<Mutex<ActionLog>>` | state.rs:69 |
| `AppState.metrics` | `Option<Arc<Metrics>>` | `Option<Arc<Metrics>>` | state.rs:73 |
| `AppState.manifest_cache` | `Arc<Mutex<ManifestCache>>` | `Arc<Mutex<ManifestCache>>` | state.rs:84 |
| `AppState.watch_shutdown_tx` | `Option<watch::Sender<bool>>` | `Option<tokio::sync::watch::Sender<bool>>` | state.rs:75 |
| `AppState.watch_reload_tx` | `Option<watch::Sender<bool>>` | `Option<tokio::sync::watch::Sender<bool>>` | state.rs:77 |
| `AppState.watch_status` | `Arc<Mutex<WatchStatus>>` | `Arc<Mutex<WatchStatus>>` | state.rs:79 |
| `AppState.config_path` | `PathBuf` | `PathBuf` | state.rs:81 |
| `RunningOp.id` | `u64` | `u64` | state.rs:89 |
| `RunningOp.command` | `String` | `String` | state.rs:90 |
| `RunningOp.cancel_token` | `CancellationToken` | `CancellationToken` | state.rs:91 |
| `RunningOp._permit` | `OwnedSemaphorePermit` | `OwnedSemaphorePermit` | state.rs:93 |
| `PidLock.path` | `PathBuf` | `PathBuf` | lock.rs:28 |
| `LockScope` | enum | `enum { Backup(String), Global, None }` | lock.rs:101 |
| `ActionLog.entries` | `VecDeque<ActionEntry>` | `VecDeque<ActionEntry>` | actions.rs:51 |
| `ActionLog.next_id` | `u64` | `u64` | actions.rs:53 |
| `ActionStatus` | enum | `enum { Running, Completed, Failed(String), Killed }` | actions.rs:14 |
| `ChBackupError` | enum | `enum { ClickHouseError(String), S3Error(String), ConfigError(String), LockError(String), BackupError(String), RestoreError(String), ManifestError(String), IoError(io::Error) }` | error.rs:5 |
| `CancellationToken` | tokio_util type | `tokio_util::sync::CancellationToken` | state.rs:14 |
| `Config.retention.backups_to_keep_local` | `i32` | `i32` | config.rs:392 |
| `Config.retention.backups_to_keep_remote` | `i32` | `i32` | config.rs:396 |
| `Config.general.backups_to_keep_remote` | `i32` | `i32` | config.rs:55 |
| `Config.api.allow_parallel` | `bool` | `bool` | config.rs (ApiConfig) |
| `WatchContext.config` | `Arc<Config>` | `Arc<Config>` | watch/mod.rs |
| `WatchContext.ch` | `ChClient` | `ChClient` | watch/mod.rs |
| `WatchContext.s3` | `S3Client` | `S3Client` | watch/mod.rs |

## Key API Signatures Verified

| Function | Signature | Location |
|---|---|---|
| `PidLock::acquire` | `pub fn acquire(path: &Path, command: &str) -> Result<Self, ChBackupError>` | lock.rs:36 |
| `lock_for_command` | `pub fn lock_for_command(command: &str, backup_name: Option<&str>) -> LockScope` | lock.rs:116 |
| `lock_path_for_scope` | `pub fn lock_path_for_scope(scope: &LockScope) -> Option<PathBuf>` | lock.rs:133 |
| `is_pid_alive` | `fn is_pid_alive(pid: u32) -> bool` | lock.rs:149 |
| `AppState::try_start_op` | `pub async fn try_start_op(&self, command: &str) -> Result<(u64, CancellationToken), &'static str>` | state.rs:150 |
| `AppState::finish_op` | `pub async fn finish_op(&self, id: u64)` | state.rs:180 |
| `AppState::fail_op` | `pub async fn fail_op(&self, id: u64, error: String)` | state.rs:194 |
| `AppState::kill_current` | `pub async fn kill_current(&self) -> bool` | state.rs:210 |
| `CancellationToken::new` | `pub fn new() -> CancellationToken` | tokio_util |
| `CancellationToken::cancel` | `pub fn cancel(&self)` | tokio_util |
| `CancellationToken::is_cancelled` | `pub fn is_cancelled(&self) -> bool` | tokio_util |
| `CancellationToken::cancelled` | `pub async fn cancelled(&self)` | tokio_util |
| `effective_retention_local` | `pub fn effective_retention_local(config: &Config) -> i32` | list.rs:704 |
| `effective_retention_remote` | `pub fn effective_retention_remote(config: &Config) -> i32` | list.rs:717 |
| `retention_remote` | `pub async fn retention_remote(s3: &S3Client, keep: i32) -> Result<usize>` | list.rs |
| `Config::load` | `pub fn load(path: &Path, env_overrides: &[String]) -> Result<Config>` | config.rs |
| `Config::validate` | `pub fn validate(&self) -> Result<()>` | config.rs |
| `apply_config_reload` | `fn apply_config_reload(ctx: &mut WatchContext)` | watch/mod.rs:604 |
| `backup::create` | `pub async fn create(config: &Config, ch: &ChClient, backup_name: &str, ...) -> Result<BackupManifest>` | backup/mod.rs |
| `upload::upload` | `pub async fn upload(config: &Config, s3: &S3Client, backup_name: &str, backup_dir: &Path, delete_local: bool, diff_from_remote: Option<&str>, resume: bool) -> Result<()>` | upload/mod.rs |

## Type Anti-Pattern Checks

- No `.as_str()` on enum types -- N/A in this plan
- No implicit String -> Enum conversions -- N/A
- CancellationToken is `Clone` (verified: state.rs uses `token.clone()`)
- Semaphore::MAX_PERMITS is `usize` constant (verified: state.rs:105)
- `tokio::sync::Mutex` used throughout (not `std::sync::Mutex`) -- verified at state.rs:13
