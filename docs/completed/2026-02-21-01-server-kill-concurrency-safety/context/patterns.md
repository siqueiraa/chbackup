# Pattern Discovery

## Global Patterns
No `docs/patterns/` directory exists. Full local discovery required.

## Pattern 1: Operation Lifecycle in Route Handlers

**Source:** `src/server/routes.rs` (every operation endpoint)

**Structure:**
```
1. Parse request body (Option<Json<T>>, unwrap_or_default)
2. try_start_op(command) -> (id, _token) -- token is DISCARDED (bug)
3. Clone state
4. tokio::spawn(async move { ... })
   a. Load config/ch/s3 from ArcSwap
   b. Call operation function
   c. Record duration + metrics
   d. finish_op(id) or fail_op(id, error)
5. Return Json(OperationStarted { id, status: "started" })
```

**Key Observation:** The `_token` (CancellationToken) is created but NEVER passed to the spawned task. It is immediately dropped after try_start_op returns. The cancellation token stored in RunningOp.cancel_token IS the same clone (via token.clone()), but the operation functions (backup::create, upload::upload, etc.) do NOT accept or check a CancellationToken parameter.

**Instances Found:** create_backup (line 601), upload_backup (line 694), download_backup (line 777), restore_backup (line 844), create_remote (line 952), restore_remote (line 1082), delete_backup (line 1227), clean_remote_broken (line 1296), clean_local_broken (line 1357), clean (line 1433), post_actions (line 243).

## Pattern 2: AppState Operation Management

**Source:** `src/server/state.rs`

**Structure:**
- `try_start_op(command)`: Acquires semaphore permit, creates CancellationToken, logs start in ActionLog, stores RunningOp
- `finish_op(id)`: Marks completed in ActionLog, clears current_op (if matching ID)
- `fail_op(id, error)`: Marks failed in ActionLog, clears current_op (if matching ID)
- `kill_current()`: Takes current_op, cancels token, marks killed in ActionLog

**Key Observation:** `current_op: Arc<Mutex<Option<RunningOp>>>` is a SINGLE slot. When allow_parallel=true, multiple ops can run but only the LAST one is tracked in current_op. Previous RunningOps are silently overwritten. This means kill_current() can only kill the most recently started operation.

## Pattern 3: PID Lock Acquisition (CLI)

**Source:** `src/main.rs:114-135`

**Structure:**
```
1. Determine command name and backup name
2. lock_for_command(cmd_name, bak_name) -> LockScope
3. lock_path_for_scope(&scope) -> Option<PathBuf>
4. PidLock::acquire(path, cmd_name) -> Result<PidLock>
5. Execute command
6. PidLock dropped automatically
```

**Key Observation:** The server command gets `LockScope::None`, meaning API operations run WITHOUT PID locks. The design doc (line 945) says PID locks prevent concurrent operations -- but the server uses only the semaphore, not PID locks. This means CLI + API concurrent operations are NOT serialized.

## Pattern 4: Watch Loop Orchestration

**Source:** `src/watch/mod.rs:330-540`

**Structure:**
```
1. Resume: list_remote -> resume_state -> decide
2. Create: backup::create(config, ch, ...)
3. Upload: upload::upload(config, s3, ...)
4. Delete local (if configured)
5. Retention: retention_local + retention_remote
6. Sleep (interruptible)
```

**Key Observation:** Watch loop calls retention after upload (step 5). But direct `upload` command via API/CLI does NOT call retention. Design doc section 3.6 step 7 says "Apply retention: delete oldest remote backups exceeding backups_to_keep_remote" after upload.

## Pattern 5: Config Hot-Reload

**Source:** `src/watch/mod.rs:604-642` and `src/server/routes.rs:1490-1506`

**Structure (watch):**
```
1. Config::load(path) -> validate() -> replace ctx.config
2. Log old->new values for key params
3. Does NOT recreate ChClient or S3Client
```

**Structure (API reload):**
```
1. If watch_reload_tx exists: send reload signal
2. If no watch: acknowledge but do nothing meaningful
3. Does NOT reload config into AppState.config
```

**Key Observation:** The API reload endpoint does NOT update AppState.config when watch is inactive. It just returns "reloaded" without doing anything. When watch IS active, it only updates the WatchContext.config -- not the AppState.config used by other handlers.

## Pattern 6: Backup Name -> Path Construction

**Source:** `src/backup/mod.rs:281-283`, `src/list.rs:500`, `src/server/routes.rs:709`

**Structure:**
```rust
let backup_dir = PathBuf::from(&config.clickhouse.data_path)
    .join("backup")
    .join(backup_name);   // NO validation on backup_name
```

**Key Observation:** The backup_name is used directly in path construction without any validation. A name like `../../etc/passwd` or `../../../tmp/malicious` would create path traversal. In the API, backup names come from:
- URL path parameters (e.g., `/api/v1/upload/{name}`)
- JSON request body (e.g., `CreateRequest.backup_name`)
- POST /api/v1/actions command string (space-delimited)
