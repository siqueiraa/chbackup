# Git Context and History

## Recent Repository History (last 20 commits)

```
c11d7794 chore: remove debug markers from list.rs
86f9298f docs: update CLAUDE.md files for Phase 7 changes
13d2e371 docs(design): update design doc for genuine Phase 6 improvements
e5af1a89 feat(storage): add PutObject/UploadPart retry with exponential backoff
e097c915 feat(config): expand env var overlay to cover 54 config fields
69d1f32d fix(config): change default ch_port from 9000 to 8123
dd2495e8 fix(config): revert Phase 6 defaults to design doc values
93f55e28 Merge branch 'claude/phase6-go-parity' into master
af5c64b0 chore: update Cargo.lock for aws-sdk-sts dependency
db6337b7 docs: update CLAUDE.md for Phase 6 Go parity
44e9f076 feat(storage): add multipart CopyObject for objects exceeding 5GB
3beb3d43 feat(storage): add concurrency and object_disk_path fields to S3Client
a49afda4 feat(list): add --format flag and latest/previous backup shortcuts
4a769975 feat(backup,restore): wire freeze-by-part, partition restore, and replica sync check
ee18f3e6 feat(server): exit process when watch loop ends and watch_is_main_process is set
2725a395 feat(server): API parity with Go clickhouse-backup
539ab56e feat(list): add incremental chain protection to retention_remote
1acafed6 feat(s3): implement STS AssumeRole for cross-account access
ccc99bc2 feat(retry): wire jitter into all retry paths for Go parity
1b07362d feat(config,s3): wire Go parity defaults, ACL, storage class, debug flags
```

## File-Specific History (Files Being Modified)

### Combined history for all affected files

```
c11d7794 chore: remove debug markers from list.rs
e5af1a89 feat(storage): add PutObject/UploadPart retry with exponential backoff
a49afda4 feat(list): add --format flag and latest/previous backup shortcuts
ee18f3e6 feat(server): exit process when watch loop ends and watch_is_main_process is set
2725a395 feat(server): API parity with Go clickhouse-backup
```

### Key file last-touch mapping

| File | Last Modified | Commit |
|------|--------------|--------|
| `src/manifest.rs` | Phase 4e (approx) | RBAC field addition |
| `src/list.rs` | c11d7794 | debug markers removed |
| `src/server/routes.rs` | 2725a395 | API parity (Phase 6) |
| `src/server/mod.rs` | ee18f3e6 | watch_is_main_process exit |
| `src/server/state.rs` | 2725a395 | API parity (Phase 6) |
| `src/upload/mod.rs` | e5af1a89 | PutObject/UploadPart retry |
| `src/upload/stream.rs` | Phase 4f (approx) | Multi-format compression |
| `src/backup/mod.rs` | 4a769975 | freeze-by-part |
| `src/backup/rbac.rs` | Phase 4e (approx) | RBAC backup |
| `src/backup/collect.rs` | Phase 5 (approx) | Projection filtering |
| `src/main.rs` | a49afda4 | list format flag |

## Branch Context

- **Current branch:** `master`
- **Main branch:** `main`
- **Commits ahead of main:** Unable to compare (master may not track main directly, or they are in sync)
- **Working tree status:** Modified files include root-causes.md (staged), various target/doc files (unstaged), new .gitignore (untracked)

## Commit Convention

All recent commits follow conventional commit format:
- `feat:` for features (with scope: `storage`, `config`, `server`, `list`, `backup`, `restore`, `s3`, `retry`)
- `fix:` for bug fixes
- `docs:` for documentation
- `chore:` for maintenance

This plan's commits should follow the same pattern. Expected scopes:
- `feat(manifest):` for rbac_size/config_size fields
- `feat(server):` for tables pagination, manifest caching, SIGQUIT handler
- `feat(upload):` for streaming multipart upload

## Phase Context

- **Previous phase:** Phase 7 (Go parity gaps) -- Complete
- **Current phase:** Phase 8 (Polish & Performance) -- Planning
- **Git base for this plan:** `c11d7794` (HEAD of master)
