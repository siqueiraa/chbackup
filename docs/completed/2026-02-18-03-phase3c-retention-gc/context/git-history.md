# Git Context

## Recent Repository History (last 20 commits)

```
df21301 chore: remove debug markers for Phase 3b metrics
ea47234 docs(server): update CLAUDE.md with metrics module documentation
52b933a feat(server): instrument operation handlers with prometheus metrics
09f1603 feat(server): replace metrics_stub with real /metrics handler
9c364c1 feat(server): add metrics field to AppState with conditional creation
2e08025 feat(server): add Metrics struct with 14 prometheus metric definitions
b0fa80e feat(deps): add prometheus 0.13 dependency for Phase 3b metrics
3d6913e feat: add API server module (Phase 3a)
bc7fcd4 style: apply cargo fmt to main.rs, update Cargo.lock
1de0664 docs: Mark plan as COMPLETED
816cc97 style: apply cargo fmt formatting to server module files
ef47897 docs(server): add CLAUDE.md for server module, update clickhouse CLAUDE.md
6229910 feat(server): wire Command::Server to start_server in main.rs
c85176f feat(server): add auto-resume for interrupted operations on restart
3766673 feat(clickhouse): add integration table DDL methods for API server
e76f3a9 feat(server): add router assembly and server startup with TLS support
df76cd9 feat(server): add delete, clean, kill, and stub endpoints
9c9abdf feat(server): add backup operation endpoints for create, upload, download, restore, create_remote, restore_remote
3bcfa44 feat(server): add read-only route handlers for health, version, status, actions, and list
6456529 feat(server): add Basic auth middleware for API endpoints
```

## File-Specific History

### src/list.rs (7 commits)

```
e8c2c4c feat(deps): add axum, tower-http, base64 dependencies and derive Serialize on BackupSummary
4300d5e style: apply cargo fmt formatting across all modules
de99468 feat(list): add broken_reason to BackupSummary and implement clean_broken
1050619 style: apply cargo fmt formatting across all modules
97cb284 feat(upload): add mixed disk upload with CopyObject for S3 disk parts
88af4c3 feat(list): implement list command with local dir scan and remote S3 listing
1bb4bc2 feat: add Phase 1 dependencies, error variants, and module declarations
```

Last substantial change was `de99468` (clean_broken implementation). The module has been stable since Phase 2.

### src/server/routes.rs (7 commits)

```
df21301 chore: remove debug markers for Phase 3b metrics
52b933a feat(server): instrument operation handlers with prometheus metrics
09f1603 feat(server): replace metrics_stub with real /metrics handler
816cc97 style: apply cargo fmt formatting to server module files
df76cd9 feat(server): add delete, clean, kill, and stub endpoints
9c9abdf feat(server): add backup operation endpoints for create, upload, download, restore, create_remote, restore_remote
3bcfa44 feat(server): add read-only route handlers for health, version, status, actions, and list
```

The `clean_stub` was added in `df76cd9` and has not been modified since. Phase 3b (metrics) instrumented all other operation handlers but left the stub untouched.

### src/main.rs (10 commits)

```
bc7fcd4 style: apply cargo fmt to main.rs, update Cargo.lock
6229910 feat(server): wire Command::Server to start_server in main.rs
6631e92 feat(cli): wire --resume and --partitions flags, implement clean_broken dispatch
bb1c0e3 feat(backup): add partition-level backup via --partitions flag
1e44ff6 feat(restore): add resume support with system.parts query
79a5d85 feat(download): add resume state, CRC64 verification, and disk space pre-flight
8d63844 feat(upload): add resume state tracking and atomic manifest upload
1050619 style: apply cargo fmt formatting across all modules
dfe3541 feat: implement create_remote command and wire --diff-from/--diff-from-remote
6d695f4 feat(cli): wire all Phase 1 commands in main.rs match arms
```

The `Command::Clean` stub was wired in `6d695f4` and has not been touched since.

## Branch Context

- **Current branch:** master
- **Commits ahead of main:** N/A (master is the main branch equivalent; `main` branch tracks same)
- **Working tree status:** Clean (only Cargo.lock modified + target/ untracked)

## Phase Sequencing Context

- **Phase 3a** (API server): Complete (3d6913e)
- **Phase 3b** (Prometheus metrics): Complete (df21301)
- **Phase 3c** (Retention/GC): **This plan** -- next in sequence
- **Phase 3d** (Watch mode): After this plan

All prerequisites are met. The API server is fully operational with metrics instrumentation. The stub endpoints for `clean` are wired and ready to be replaced.
