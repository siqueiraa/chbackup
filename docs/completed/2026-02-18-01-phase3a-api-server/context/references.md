# Symbol and Reference Analysis

## Phase 1: MCP/LSP Symbol Analysis

### Key Types and Their Locations

#### Config Types (all verified via LSP documentSymbol)

| Type | File | Line | Derives |
|------|------|------|---------|
| `Config` | `src/config.rs` | 5 | Debug, Clone, Default, Serialize, Deserialize |
| `ApiConfig` | `src/config.rs` | 426 | Debug, Clone, Serialize, Deserialize |
| `WatchConfig` | `src/config.rs` | 392 | Debug, Clone, Serialize, Deserialize |

#### ApiConfig Fields (verified via LSP hover, all fields have defaults)

| Field | Type | Default | Design Section |
|-------|------|---------|---------------|
| `listen` | `String` | `"localhost:7171"` | S9 |
| `enable_metrics` | `bool` | `true` | S9 |
| `create_integration_tables` | `bool` | `true` | S9.1 |
| `integration_tables_host` | `String` | `""` | S9.1 |
| `username` | `String` | `""` | S9 |
| `password` | `String` | `""` | S9 |
| `secure` | `bool` | `false` | S9 |
| `certificate_file` | `String` | `""` | S9 |
| `private_key_file` | `String` | `""` | S9 |
| `ca_cert_file` | `String` | `""` | S9 |
| `allow_parallel` | `bool` | `false` | S9 |
| `complete_resumable_after_restart` | `bool` | `true` | S9 |
| `watch_is_main_process` | `bool` | `false` | S10 |

#### Client Types (verified via LSP hover)

| Type | File | Derives/Traits | Notes |
|------|------|----------------|-------|
| `ChClient` | `src/clickhouse/client.rs:12` | `Clone` | Wraps `clickhouse::Client` |
| `S3Client` | `src/storage/s3.rs:25` | `Clone, Debug` | Wraps `aws_sdk_s3::Client` |

#### Command Entry Points (verified via LSP hover, exact signatures)

```rust
// backup::create
pub async fn create(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    diff_from: Option<&str>,
    partitions: Option<&str>,
    skip_check_parts_columns: bool,
) -> Result<BackupManifest>

// upload::upload
pub async fn upload(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    backup_dir: &Path,
    delete_local: bool,
    diff_from_remote: Option<&str>,
    resume: bool,
) -> Result<()>

// download::download
pub async fn download(
    config: &Config,
    s3: &S3Client,
    backup_name: &str,
    resume: bool,
) -> Result<PathBuf>

// restore::restore
pub async fn restore(
    config: &Config,
    ch: &ChClient,
    backup_name: &str,
    table_pattern: Option<&str>,
    schema_only: bool,
    data_only: bool,
    resume: bool,
) -> Result<()>
```

#### List Module Functions (verified via LSP)

```rust
pub async fn list(data_path: &str, s3: &S3Client, location: Option<&Location>) -> Result<()>
pub fn list_local(data_path: &str) -> Result<Vec<BackupSummary>>
pub async fn list_remote(s3: &S3Client) -> Result<Vec<BackupSummary>>
pub async fn delete(data_path: &str, s3: &S3Client, location: &Location, backup_name: &str) -> Result<()>
pub fn delete_local(data_path: &str, backup_name: &str) -> Result<()>
pub async fn delete_remote(s3: &S3Client, backup_name: &str) -> Result<()>
pub fn clean_broken_local(data_path: &str) -> Result<usize>
pub async fn clean_broken_remote(s3: &S3Client) -> Result<usize>
pub async fn clean_broken(data_path: &str, s3: &S3Client, location: &Location) -> Result<()>
```

#### BackupSummary (verified via LSP documentSymbol)

```rust
pub struct BackupSummary {
    pub name: String,                       // Line 26
    pub timestamp: Option<DateTime<Utc>>,   // Line 28
    pub size: u64,                          // Line 30
    pub compressed_size: u64,               // Line 32
    pub table_count: usize,                 // Line 34
    pub is_broken: bool,                    // Line 36
    pub broken_reason: Option<String>,      // Line 38
}
// Current derives: Debug, Clone
// MISSING: Serialize -- needed for JSON API responses
```

#### ChClient DDL Methods (verified via grep + LSP)

```rust
pub async fn execute_ddl(&self, ddl: &str) -> Result<()>     // Line 398
pub async fn get_version(&self) -> Result<String>             // Line 358
pub async fn database_exists(&self, db: &str) -> Result<bool> // Line 403
pub async fn table_exists(&self, db: &str, table: &str) -> Result<bool> // Line 425
```

#### Lock Types (verified via LSP)

```rust
pub enum LockScope { Backup(String), Global, None }
pub fn lock_for_command(command: &str, backup_name: Option<&str>) -> LockScope
pub fn lock_path_for_scope(scope: &LockScope) -> Option<PathBuf>
pub struct PidLock { path: PathBuf }
impl PidLock { pub fn acquire(path: &Path, command: &str) -> Result<Self, ChBackupError> }
```

#### Resume Types (verified via LSP)

```rust
pub struct UploadState { completed_keys: HashSet<String>, backup_name: String, params_hash: String }
pub struct DownloadState { completed_keys: HashSet<String>, backup_name: String, params_hash: String }
pub struct RestoreState { attached_parts: HashMap<String, Vec<String>>, backup_name: String }
pub fn load_state_file<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>>
pub fn save_state_graceful<T: Serialize>(path: &Path, state: &T)
pub fn delete_state_file(path: &Path)
```

## Phase 1.5: Call Hierarchy Analysis

### Callers of Command Entry Points (incomingCalls)

All command functions are currently called ONLY from `src/main.rs`:
- `backup::create` -- called from `Command::Create` and `Command::CreateRemote` arms
- `upload::upload` -- called from `Command::Upload` and `Command::CreateRemote` arms
- `download::download` -- called from `Command::Download` arm
- `restore::restore` -- called from `Command::Restore` arm
- `list::list` -- called from `Command::List` arm
- `list::delete` -- called from `Command::Delete` arm
- `list::clean_broken` -- called from `Command::CleanBroken` arm

**Impact:** The server handlers will become ADDITIONAL callers of these same functions. No existing callers need modification.

### Outgoing Calls from Command Functions

Each command function internally:
1. Creates clients if needed (ChClient, S3Client)
2. Calls sub-module functions (freeze, collect, compress, etc.)
3. Returns Result

**Impact:** The server does NOT need to understand the internal details of each command. It just calls the top-level function and handles success/error.

## Phase 2: Reference Analysis -- Symbols Being Modified

### src/main.rs -- Command::Server arm
- **Current:** Stub that logs "server: not implemented in Phase 1"
- **Change:** Will call `server::start_server(config).await`
- **Callers of main:** N/A (binary entry point)

### src/lib.rs -- Module declarations
- **Current:** 17 pub mod declarations (backup through upload)
- **Change:** Add `pub mod server`
- **Impact:** No existing code affected

### src/list.rs -- BackupSummary
- **Current derives:** `Debug, Clone`
- **Change:** Add `Serialize` derive for JSON API responses
- **Impact:** All consumers of BackupSummary unaffected (Serialize adds no constraints)

### Cargo.toml
- **Change:** Add axum, tower-http, base64, axum-server dependencies
- **Impact:** Build time increase, no code changes needed

### src/clickhouse/client.rs -- New methods
- **Change:** Add `create_integration_tables()` and `drop_integration_tables()` methods
- **Impact:** No existing code affected (new methods only)

## Cross-Reference: Design Doc Endpoints vs Existing Functions

| API Endpoint | HTTP Method | Existing Function | Needs ChClient | Needs S3Client |
|-------------|-------------|-------------------|----------------|----------------|
| `/api/v1/create` | POST | `backup::create` | YES | NO |
| `/api/v1/create_remote` | POST | `backup::create` + `upload::upload` | YES | YES |
| `/api/v1/upload/{name}` | POST | `upload::upload` | NO | YES |
| `/api/v1/download/{name}` | POST | `download::download` | NO | YES |
| `/api/v1/restore/{name}` | POST | `restore::restore` | YES | NO |
| `/api/v1/restore_remote/{name}` | POST | `download::download` + `restore::restore` | YES | YES |
| `/api/v1/list` | GET | `list::list_local` + `list::list_remote` | NO | YES |
| `/api/v1/tables` | GET | Not implemented (stub) | YES | NO |
| `/api/v1/status` | GET | New (in-memory state) | NO | NO |
| `/api/v1/actions` | GET | New (action log) | NO | NO |
| `/api/v1/version` | GET | `ChClient::get_version` + binary version | YES | NO |
| `/api/v1/clean` | POST | Not implemented (stub) | NO | NO |
| `/api/v1/clean/remote_broken` | POST | `list::clean_broken_remote` | NO | YES |
| `/api/v1/clean/local_broken` | POST | `list::clean_broken_local` | NO | NO |
| `/api/v1/kill` | POST | New (CancellationToken) | NO | NO |
| `/api/v1/reload` | POST | New (config reload) | NO | NO |
| `/api/v1/restart` | POST | New (rebind socket) | NO | NO |
| `/api/v1/watch/start` | POST | Not implemented (Phase 3d) | NO | NO |
| `/api/v1/watch/stop` | POST | Not implemented (Phase 3d) | NO | NO |
| `/api/v1/watch/status` | GET | Not implemented (Phase 3d) | NO | NO |
| `/health` | GET | New (static 200) | NO | NO |
| `/metrics` | GET | Not implemented (Phase 3b) | NO | NO |
| `DELETE /api/v1/delete/{where}/{name}` | DELETE | `list::delete` | NO | YES |

## Key Observations

1. **BackupSummary needs Serialize** -- Currently missing, required for `/api/v1/list` JSON response
2. **Tables command is a stub** -- `Command::Tables` is not implemented; API handler can also return a stub or "not implemented" for now
3. **Watch endpoints are Phase 3d** -- Server framework must route them but handlers can return "not implemented" initially
4. **Metrics is Phase 3b** -- `/metrics` endpoint can return empty or stub
5. **Clean command is a stub** -- `Command::Clean` is not implemented in main.rs
6. **All command functions take `&Config`** -- Config must be in shared state (Arc)
7. **ChClient and S3Client are Clone** -- Can be constructed once and shared across handlers
8. **resume::load_state_file is sync** -- Can be used from async context to scan for resumable state files
