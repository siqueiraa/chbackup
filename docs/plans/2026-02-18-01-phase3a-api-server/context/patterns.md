# Pattern Discovery

## Global Patterns Registry
No `docs/patterns/` directory exists. Full pattern discovery performed locally.

## Component Identification

### New Components (Phase 3a)
1. **HTTP Server** -- axum Router bound to `api.listen` address
2. **AppState** -- Shared state across all handlers (config, clients, action log, cancellation)
3. **Route Handlers** -- One handler per API endpoint, delegating to existing command functions
4. **Action Log** -- In-memory ring buffer tracking recent operations (start, finish, status, error)
5. **Auth Middleware** -- HTTP Basic auth layer (when api.username/api.password configured)
6. **TLS Support** -- axum-server with rustls or native-tls for HTTPS
7. **Integration Tables** -- SQL DDL to create/drop URL engine tables in ClickHouse
8. **Operation Serialization** -- Mutex or RwLock to enforce sequential ops (when allow_parallel=false)
9. **CancellationToken** -- tokio_util CancellationToken per running operation for POST /kill
10. **Auto-Resume** -- On startup, scan for *.state.json and queue resume operations

### Existing Components Reused
1. **Config** (`config.rs`) -- `Config`, `ApiConfig`, `WatchConfig` already defined
2. **ChClient** (`clickhouse/client.rs`) -- ClickHouse queries for integration tables
3. **S3Client** (`storage/s3.rs`) -- S3 operations for all backup commands
4. **backup::create** -- Backup creation logic
5. **upload::upload** -- Upload logic
6. **download::download** -- Download logic
7. **restore::restore** -- Restore logic
8. **list::list_local/list_remote** -- List backups
9. **list::delete** -- Delete backups
10. **list::clean_broken** -- Clean broken backups
11. **PidLock** (`lock.rs`) -- Per-backup locking
12. **resume** (`resume.rs`) -- Resume state types and helpers

## Reference Implementation Analysis

### Pattern 1: Command Delegation (from main.rs)
Each CLI command follows identical structure:
1. Parse flags from CLI
2. Create ChClient and/or S3Client from config
3. Call the module's entry function with config + clients + params
4. Log completion

**Example (create):**
```rust
let ch = ChClient::new(&config.clickhouse)?;
let _manifest = backup::create(
    &config, &ch, &name,
    tables.as_deref(), schema,
    diff_from.as_deref(), partitions.as_deref(),
    skip_check_parts_columns,
).await?;
```

The API server will use this same pattern: extract params from JSON body, create clients, call the function.

### Pattern 2: Error Propagation (anyhow)
All command functions return `anyhow::Result<T>`. The server must convert these to HTTP error responses:
- Success -> 200 OK with JSON body
- Error -> 500 with error message in JSON

### Pattern 3: Parallel Task Spawning (from upload/download/restore)
Uses `tokio::spawn` + `Arc<Semaphore>` + `futures::try_join_all`. The server will use the same pattern for background operation execution: spawn the operation, track it in the action log, allow cancellation.

### Pattern 4: PidLock Scoping (from lock.rs)
- Backup-scoped: `/tmp/chbackup.{name}.pid`
- Global: `/tmp/chbackup.global.pid`
- None: read-only commands

The server will need to manage locks differently: instead of a single CLI lock, it must handle concurrent API requests. When `allow_parallel=false`, operations are serialized via an async Mutex. When true, per-backup PidLocks prevent concurrent operations on the same backup.

### Pattern 5: Resume State (from resume.rs)
State files: `{backup_dir}/upload.state.json`, `{backup_dir}/download.state.json`, `{backup_dir}/restore.state.json`. Auto-resume on restart scans for these files and queues the corresponding operations.

## Axum Patterns (External Crate -- axum 0.7)

### State Sharing
```rust
#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    // ...
}
let app = Router::new()
    .route("/path", get(handler))
    .with_state(app_state);

async fn handler(State(state): State<AppState>) -> impl IntoResponse { ... }
```

### Middleware (tower layers)
```rust
// Basic auth via tower-http or custom middleware
let app = Router::new()
    .layer(middleware::from_fn_with_state(state, auth_layer));
```

### JSON request/response
```rust
use axum::Json;
async fn create_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateRequest>,
) -> Result<Json<ActionResponse>, StatusCode> { ... }
```

## File Layout for src/server/

```
src/server/
  mod.rs          -- build_router(), start_server(), AppState
  routes.rs       -- All endpoint handler functions
  actions.rs      -- ActionLog ring buffer + ActionEntry types
  auth.rs         -- Basic auth middleware
  state.rs        -- Operation state management (serialization, cancellation)
```
