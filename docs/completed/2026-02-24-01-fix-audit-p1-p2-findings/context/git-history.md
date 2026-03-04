# Git History Context

## Recent Repository History (last 20 commits)

```
8bdd38ff merge: 2026-02-23-01-correctness-fixes - fix 7 correctness issues from audit
5fa70173 docs: Archive completed plan 2026-02-23-01-correctness-fixes
ae2227df docs: Mark plan as COMPLETED
8ada5713 style: apply cargo fmt to fix import ordering and line length
6a04106d docs: MR review PASS
cedd2681 docs: update CLAUDE.md for path_encoding, disable_ssl/cert_verification, check_parts_columns, env-style --env
a1c8be25 fix(s3): disable_cert_verification forces HTTP endpoint (remove broken CA_BUNDLE approach)
17b6f855 refactor: replace duplicated url_encode with canonical path_encoding module
9ee9b616 fix: path_encoding module, disable_ssl wiring, strict check_parts_columns, env-style --env keys
cbf1a3d2 test(storage): hermetic S3 unit tests (mock_s3_fields, #[ignore] network tests)
746648c2 docs: patch plan 2026-02-23-01-correctness-fixes per review feedback
1ebbbd34 docs: Create plan 2026-02-23-01-correctness-fixes
5fc70108 docs: Archive completed plan 2026-02-22-01-remove-dead-code-arc-locks
7c0b1d29 docs: Mark plan as COMPLETED
432e7829 docs: MR review PASS
fbc227e4 docs: update CLAUDE.md files to reflect dead code removal from ChClient, S3Client, and attach.rs
c6152ac8 refactor(restore): remove dead attach_parts() function superseded by attach_parts_owned()
9edc6e3c refactor(storage): remove unused inner(), concurrency(), object_disk_path() getters and dead fields from S3Client
5b912ba4 refactor(clickhouse): remove dead debug field and unused inner() getter from ChClient
33377878 fix(server): lock-order inversion in status() handler
```

## File-Specific History (files being modified)

```
$ git log --oneline -10 -- src/main.rs src/cli.rs src/list.rs src/backup/mod.rs src/lock.rs

9ee9b616 fix: path_encoding module, disable_ssl wiring, strict check_parts_columns, env-style --env keys
5fc70108 docs: Archive completed plan 2026-02-22-01-remove-dead-code-arc-locks
a7503b3a chore: apply cargo fmt to lock.rs, main.rs, state.rs
3973d090 docs(main): clarify create --resume as intentionally deferred design decision
59f08788 feat(list): add auto-retention after upload for CLI and API handlers
ccbc36a1 fix(lock): eliminate TOCTOU race in PidLock::acquire via O_CREAT|O_EXCL
f13aebd0 feat(server): add backup name path traversal validation
10472448 feat(list): add object_disk_size and required fields to BackupSummary
b3036c56 chore: apply cargo fmt across all modified files
2c4dca91 feat(backup): use per-disk staging dirs in collect_parts()
```

### Notable Recent Changes Affecting This Plan

1. **`3973d090`**: Added documentation comment in main.rs clarifying `create --resume` as intentionally deferred. This validates the P2 finding about design mismatch being a known/documented decision.

2. **`ccbc36a1`**: Fixed TOCTOU race in PidLock::acquire via O_CREAT|O_EXCL. The lock mechanism itself is sound; the issue in P1 is about WHAT name is locked, not HOW it is locked.

3. **`f13aebd0`**: Added `validate_backup_name()` -- validates name BEFORE lock acquisition. Note: `validate_backup_name("latest")` returns `Ok(())` since "latest" contains no `..`, `/`, `\`, or NUL. This is correct behavior (latest is a valid string), but it means the validation does NOT catch shortcut names.

4. **`9ee9b616`**: Path encoding module introduced. Doctests pass in this version. Confirms P2 doctest finding is stale.

## Branch Context

```
Current branch: master
Main branch: main
Commits ahead of main: 0 (on master, not main)
```

The repository uses `master` as the working branch (per git status). The `main` branch is the merge target for PRs.

## Relevant Patterns from History

1. **Bug fix pattern**: Fixes use `fix:` or `fix(module):` prefix
2. **Multi-file fixes**: Often committed together (e.g., `9ee9b616` touched 5+ files)
3. **Style pass**: `cargo fmt` applied after implementation commits
4. **Documentation alignment**: Separate commits for CLAUDE.md updates
