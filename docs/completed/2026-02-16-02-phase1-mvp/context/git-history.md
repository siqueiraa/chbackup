# Git History — Phase 1 MVP

## Branch Context

- **Current branch:** master
- **Main branch:** main
- **Commits ahead of main:** Unable to compare (main branch may not have diverged or doesn't exist locally)
- **Working tree status:** Clean (only `target/` untracked)

## Recent Repository History

```
880a640 chore: apply rustfmt formatting fixes
b70c455 feat: add config.example.yml and wire full command flow
5cc69c9 feat(storage): add S3Client wrapper with config-driven setup and ping
d5949b6 feat(clickhouse): add ChClient wrapper with config-driven setup and ping
764f2fe feat(cli): wire default-config and print-config commands
77d825e feat(config): add configuration loader with ~106 params, env overlay, and validation
a3ae11d feat(lock): add PID lock with three-tier scope (backup/global/none)
48929e8 feat(logging): add init_logging with text/JSON mode selection
bc9b235 feat(cli): add all 15 subcommands with full flag sets from design doc
c236b4e feat: initialize cargo project with dependencies and error types
7c9906a chore: remove docs from tracking
d443682 docs: add design and roadmap documents
```

## File-Specific History

### Files Being Modified

| File | Last Modified In | Commits |
|------|-----------------|---------|
| src/main.rs | b70c455, 880a640 | Wire full command flow, format fixes |
| src/lib.rs | b70c455 | Add module declarations |
| src/clickhouse/client.rs | d5949b6, 880a640 | Initial ChClient, format fixes |
| src/storage/s3.rs | 5cc69c9, 880a640 | Initial S3Client, format fixes |
| src/error.rs | c236b4e | Initial error types |
| src/config.rs | 77d825e, 880a640 | Full config loader, format fixes |
| src/cli.rs | bc9b235, b70c455, 880a640 | All subcommands, command flow, format fixes |
| Cargo.toml | c236b4e, d5949b6, 5cc69c9, 880a640 | Dependencies added progressively |

### Files Being Created (New in Phase 1)

| File | Purpose |
|------|---------|
| src/manifest.rs | BackupManifest, TableManifest, PartInfo structs |
| src/table_filter.rs | Glob pattern matching for -t flag |
| src/list.rs | Local + remote backup listing |
| src/backup/mod.rs | Backup orchestration entry point |
| src/backup/freeze.rs | FREEZE/UNFREEZE operations |
| src/backup/mutations.rs | Pending mutation check |
| src/backup/sync_replica.rs | SYSTEM SYNC REPLICA |
| src/backup/checksum.rs | CRC64 checksum computation |
| src/backup/collect.rs | Shadow directory walk, part collection |
| src/upload/mod.rs | Upload orchestration entry point |
| src/upload/stream.rs | Streaming compress + S3 PUT pipeline |
| src/download/mod.rs | Download orchestration entry point |
| src/download/stream.rs | S3 GET + streaming decompress pipeline |
| src/restore/mod.rs | Restore orchestration entry point |
| src/restore/schema.rs | CREATE TABLE/DATABASE from DDL |
| src/restore/attach.rs | Hardlink + ATTACH PART + chown |
| src/restore/sort.rs | SortPartsByMinBlock |

## Phase 0 Completion Summary

Phase 0 (Skeleton) is fully complete. All deliverables from the roadmap are implemented:
- CLI skeleton with all 15 subcommands and full flag sets
- Config loader with ~106 params, env overlay, CLI overrides, validation
- ClickHouse client wrapper (HTTP interface via clickhouse-rs)
- S3 client wrapper (aws-sdk-s3 with custom endpoint, force_path_style)
- PID lock with three-tier scope (backup/global/none)
- Logging with text/JSON mode selection
- Error types (thiserror enum with 5 variants)
- 14 tests passing (9 unit + 5 integration)

## Commit Convention

All commits follow conventional commit format:
- `feat:` / `feat(scope):` for new features
- `chore:` for maintenance (formatting, etc.)
- `docs:` for documentation
- No AI/Claude references in commit messages

## Working Directory State

```
?? target/
```

Only `target/` is untracked (build artifacts, gitignored). No uncommitted changes to source files.
