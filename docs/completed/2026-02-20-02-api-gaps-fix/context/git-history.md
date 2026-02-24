# Git Context

## Current Branch

- **Branch:** `master`
- **Commits ahead of main:** 0 (on main)

## Recent Repository History (last 20 commits)

```
3a6e946d docs: update CLAUDE.md files for per-disk backup directory changes
b3036c56 chore: apply cargo fmt across all modified files
8ff5f120 feat(restore): update ATTACH TABLE mode for per-disk shadow paths
507d9a61 feat(download): update find_existing_part() for per-disk search
dd386046 feat(upload): update upload delete_local to clean per-disk dirs
8fb18c0c feat(upload): update find_part_dir() to use resolve_shadow_part_path()
2c4dca91 feat(backup): use per-disk staging dirs in collect_parts()
402389c0 feat(download): write parts to per-disk backup directories
80eb80cc feat(backup): add resolve_shadow_part_path() helper with 4-step fallback chain
452389c0 feat(list): update delete_local() to clean per-disk backup directories
afa01dab docs: Archive completed plan 2026-02-20-01-phase8-polish-performance
6244ddf5 docs: Mark plan as COMPLETED
fbf32916 docs: update CLAUDE.md files for Phase 8 changes
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
52fb1f46 feat(upload): wire streaming multipart upload for large parts
7307bb05 feat(list): implement ManifestCache with TTL-based expiry
f707ad33 feat(server): wire rbac_size and config_size through to ListResponse
620a0c08 feat(backup): compute rbac_size and config_size during backup create
fa685435 feat(upload): add streaming compression for large part multipart upload
16773248 feat(server): add SIGQUIT handler for stack dump debugging
```

## File-Specific History

### src/server/routes.rs (last 5 commits)

```
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
f707ad33 feat(server): wire rbac_size and config_size through to ListResponse
226e068f feat(server): add offset/limit pagination to tables endpoint
2725a395 feat(server): API parity with Go clickhouse-backup
6cb6f1ad feat(server): implement GET /api/v1/tables endpoint
```

### src/server/mod.rs (last 5 commits)

```
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
16773248 feat(server): add SIGQUIT handler for stack dump debugging
226e068f feat(server): add offset/limit pagination to tables endpoint
2725a395 feat(server): API parity with Go clickhouse-backup
6cb6f1ad feat(server): implement GET /api/v1/tables endpoint
```

### src/list.rs (last 5 commits)

```
452389c0 feat(list): update delete_local() to clean per-disk backup directories
7307bb05 feat(list): implement ManifestCache with TTL-based expiry
c11d7794 chore: remove debug markers from list.rs
a49afda4 feat(list): add --format flag and latest/previous backup shortcuts
cfa1f44b feat(manifest): add rbac_size and config_size fields to BackupManifest and BackupSummary
```

### src/manifest.rs (last 3 commits)

```
cfa1f44b feat(manifest): add rbac_size and config_size fields to BackupManifest and BackupSummary
<earlier commits from Phase 2c+ >
```

### src/server/state.rs

```
a5de6b80 feat(server): wire ManifestCache into AppState with TTL and invalidation
226e068f feat(server): add offset/limit pagination to tables endpoint
2725a395 feat(server): API parity with Go clickhouse-backup
```

## Relevant Patterns from History

1. **Field addition pattern (rbac_size/config_size):** Commit cfa1f44b added `rbac_size: u64` and `config_size: u64` to both `BackupManifest` and `BackupSummary` with `#[serde(default)]` for backward compatibility. All construction sites were updated. Commit f707ad33 wired them through `summary_to_list_response()`.

2. **Pagination pattern (tables endpoint):** Commit 226e068f added `offset: Option<usize>` and `limit: Option<usize>` to `TablesParams` and returned `X-Total-Count` header. Same pattern needed for list endpoint.

3. **Signal handler pattern (SIGQUIT):** Commit 16773248 added SIGQUIT handler following the same `tokio::signal::unix::{signal, SignalKind}` pattern. The handler is non-terminating (runs in a loop). Same pattern applies for SIGTERM but should trigger shutdown instead of stack dump.

4. **ManifestCache invalidation:** Commit a5de6b80 shows the pattern of invalidating cache after mutating operations (upload, delete, clean_broken_remote).

## Working Tree Status

- Modified: `.claude/skills/self-healing/references/root-causes.md` (staging area)
- Modified: `target/` docs (build artifacts, not relevant)
- Untracked: `.gitignore`
- Source files: Clean (no uncommitted source changes)
