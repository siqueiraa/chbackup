# Git Context and History

## Recent Repository History (Last 20 Commits)

```
b241320 docs: update CLAUDE.md for Phase 2b incremental backup changes
dfe3541 feat: implement create_remote command and wire --diff-from/--diff-from-remote
fcc701e feat(upload): integrate --diff-from-remote into upload() for incremental uploads
eb3e1cf feat(backup): integrate --diff-from into create() for incremental backups
2e505b2 feat(backup): add diff_parts() for incremental backup comparison
4923a70 docs: Archive completed plan 2026-02-17-01-phase2a-parallelism
63f0ab2 docs: Mark plan as COMPLETED
633ea6d docs: update CLAUDE.md for Phase 2a parallelism changes
1d44f56 feat(restore): parallelize table restore with max_connections and engine-aware ATTACH
63ad1b3 feat(download): parallelize download with flat work queue and rate limiting
f591535 feat(upload): parallelize upload with flat work queue, multipart, and rate limiting
90f9029 feat(backup): parallelize FREEZE and collect with max_connections semaphore
68949f7 feat(rate-limiter): add token-bucket rate limiter module
d4be4a5 feat(s3): add multipart upload methods and chunk size calculation
a7c0ca1 feat(concurrency): add futures crate and effective concurrency helpers
be8f01f chore: remove debug markers
caa691c docs: add CLAUDE.md for all Phase 1 modules and update root CLAUDE.md
6d695f4 feat(cli): wire all Phase 1 commands in main.rs match arms
3341479 feat(restore): implement restore module with schema creation and part attachment
324e85f feat(upload): implement upload with tar+LZ4 compression and S3 PutObject
```

## File-Specific History (Affected Files)

```
fcc701e feat(upload): integrate --diff-from-remote into upload() for incremental uploads
eb3e1cf feat(backup): integrate --diff-from into create() for incremental backups
2e505b2 feat(backup): add diff_parts() for incremental backup comparison
1d44f56 feat(restore): parallelize table restore with max_connections and engine-aware ATTACH
63ad1b3 feat(download): parallelize download with flat work queue and rate limiting
f591535 feat(upload): parallelize upload with flat work queue, multipart, and rate limiting
90f9029 feat(backup): parallelize FREEZE and collect with max_connections semaphore
68949f7 feat(rate-limiter): add token-bucket rate limiter module
d4be4a5 feat(s3): add multipart upload methods and chunk size calculation
a7c0ca1 feat(concurrency): add futures crate and effective concurrency helpers
```

## Branch Context

- **Current branch**: `master`
- **Main branch**: `main`
- **Divergence from main**: On main/no divergence (master == main HEAD)
- **Working tree**: Clean (only `Cargo.lock` modified, `target/` untracked)

## Phase Evolution

The codebase has evolved through clear phases:
1. **Phase 0** (skeleton): CLI, config, clients, logging
2. **Phase 1** (MVP): Sequential single-table backup and restore pipeline
3. **Phase 2a** (parallelism): Parallel operations, multipart upload, rate limiting
4. **Phase 2b** (incrementals): diff_parts(), --diff-from, --diff-from-remote, create_remote

**Phase 2c** (this plan) is the next phase -- S3 object disk support.

## Key Observations

1. **No merge conflicts expected**: Working tree is clean. All Phase 2a and 2b changes are committed.
2. **Commit style**: Conventional commits with scope (e.g., `feat(upload):`, `feat(backup):`).
3. **No open PRs or branches**: Development is linear on master.
4. **Recent velocity**: Phase 2a and 2b were completed in the last few commits (b241320 is latest).
