# Git History -- Phase 2b Incremental Backups

## Recent Repository History

```
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
ad4802c feat(backup): implement backup::create with FREEZE/shadow walk/hardlink/CRC64/UNFREEZE
ae4598d feat(download): implement download module with S3 fetch and LZ4 decompression
88af4c3 feat(list): implement list command with local dir scan and remote S3 listing
a381732 feat(storage): add S3Client put, get, list, delete, and head operations
8bfc841 feat(clickhouse): add FREEZE, UNFREEZE, table listing, mutations, attach, DDL methods
```

## File-Specific History

### Key files being modified

```
# git log --oneline -10 -- src/backup/mod.rs src/upload/mod.rs src/main.rs src/cli.rs src/manifest.rs
f591535 feat(upload): parallelize upload with flat work queue, multipart, and rate limiting
90f9029 feat(backup): parallelize FREEZE and collect with max_connections semaphore
be8f01f chore: remove debug markers
6d695f4 feat(cli): wire all Phase 1 commands in main.rs match arms
324e85f feat(upload): implement upload with tar+LZ4 compression and S3 PutObject
ad4802c feat(backup): implement backup::create with FREEZE/shadow walk/hardlink/CRC64/UNFREEZE
63024ea feat(manifest): add BackupManifest and related types with serde roundtrip
1bb4bc2 feat: add Phase 1 dependencies, error variants, and module declarations
b70c455 feat: add config.example.yml and wire full command flow
764f2fe feat(cli): wire default-config and print-config commands
```

### Most recent changes to each file

| File | Last Commit | Description |
|---|---|---|
| `src/backup/mod.rs` | `90f9029` | Phase 2a: parallelize FREEZE and collect |
| `src/upload/mod.rs` | `f591535` | Phase 2a: parallelize upload with multipart |
| `src/main.rs` | `6d695f4` | Phase 1: wire all commands in main.rs |
| `src/cli.rs` | `6d695f4` | Phase 1: wire all commands (includes diff-from flags) |
| `src/manifest.rs` | `63024ea` | Phase 1: add manifest types |

## Branch Context

**Current branch:** `master`
**Commits ahead of main:** 0 (master is main)

## Implementation Phase Context

- **Phase 0** (skeleton): Complete
- **Phase 1** (MVP): Complete -- sequential backup/restore pipeline
- **Phase 2a** (parallelism): Complete -- parallel operations, multipart upload, rate limiting
- **Phase 2b** (incremental): **THIS PLAN** -- `--diff-from` and `--diff-from-remote`

## Relevant Commit Patterns

The codebase follows conventional commits:
- `feat(module):` for new features
- `fix:` for bug fixes
- `refactor:` for restructuring
- `docs:` for documentation
- `chore:` for maintenance

Expected commit pattern for Phase 2b:
- `feat(backup): add --diff-from incremental comparison with CRC64 verification`
- `feat(upload): add --diff-from-remote with carried part skipping`
- `feat(create_remote): implement create_remote as create+upload composition`
