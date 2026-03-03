# Git Context and History

## Repository State

- **Current branch:** master
- **HEAD commit:** 421cc296 fix: eliminate protobuf vulnerability, add coverage pipeline, wire part counters, DRY S3 disk helpers
- **Clean working tree:** Yes (no uncommitted changes except untracked plan docs)
- **Other branches:** 6 feature branches (all merged, no active work)

## Recent Repository History (last 20 commits)

```
421cc296 fix: eliminate protobuf vulnerability, add coverage pipeline, wire part counters, DRY S3 disk helpers
c9ebd369 refactor: migrate from prometheus to prometheus-client crate
48765084 fix: restore resume idempotency, in-table parallelism, replica polling
50de942a fix: cargo fmt, graceful mutex poison, DRY tests, test-ID traceability, coverage gate
52343be9 fix: validate delete location, suppress AWS debug logs, sort list by timestamp
7b23db08 fix: millisecond backup names, sync test mutation, DRY server readiness
56c09838 fix: redact secrets in print-config and fix audit P1/P2 findings
38eebdc0 fix: consolidate Docker test path with S3 disk bootstrap and ZK healthcheck
e35d2e57 fix: clear pending list on successful retry loop completion
c2d12dc2 fix: move poll_action_completion to top of run_tests.sh
9963d1e9 fix: Dockerfile.test ARG scoping, poll_action_completion set -e safety, CI cleanup
caa30e4d test: add unit tests for pure functions and CI coverage tracking
68fe313a chore: apply cargo fmt
0914d271 chore: add coverage artifacts to .gitignore
81d722c2 fix: make Dockerfile.test self-contained with builder stage
d17c5128 fix: add empty_table to test fixtures and fix API polling false-PASS
06f7014d test: add 380+ unit tests to meet coverage gates
3aae8efb fix: validate manifest disk paths before local write/delete operations
ca87b8fc fix: rewrite classify_backup_type to use glob pattern matching
399c7566 refactor: extract DRY helpers in test harness + update out-of-sync docs
```

## Coverage-Related Commit History

| Commit | Summary |
|--------|---------|
| `421cc296` | Added coverage pipeline (CI), wired part counters |
| `50de942a` | Added coverage gate (35% minimum) |
| `caa30e4d` | Added unit tests for pure functions and CI coverage tracking |
| `06f7014d` | Added 380+ unit tests to meet coverage gates |

These commits established the current test infrastructure:
- CI coverage gate at 35% (very conservative)
- `cargo-llvm-cov` for coverage measurement
- Integration coverage merging (unit + integration profraw)

## File-Specific History for Test Targets

### src/main.rs (0% coverage)
```
421cc296 fix: eliminate protobuf vulnerability, add coverage pipeline, wire part counters, DRY S3 disk helpers
7b23db08 fix: millisecond backup names, sync test mutation, DRY server readiness
56c09838 fix: redact secrets in print-config and fix audit P1/P2 findings
3cfa9906 fix: round 3 codebase audit fixes (prior session accumulated changes)
4bc7b5cb feat: thread CancellationToken into inner spawned tasks for kill propagation
```

### src/backup/mod.rs (46.41%)
```
421cc296 fix: eliminate protobuf vulnerability, add coverage pipeline, wire part counters, DRY S3 disk helpers
50de942a fix: cargo fmt, graceful mutex poison, DRY tests, test-ID traceability, coverage gate
caa30e4d test: add unit tests for pure functions and CI coverage tracking
b0a5a9a7 feat: S3 disk backup/restore support for ClickHouse 24.8+
3cfa9906 fix: round 3 codebase audit fixes (prior session accumulated changes)
```

### src/download/mod.rs (44.27%)
```
50de942a fix: cargo fmt, graceful mutex poison, DRY tests, test-ID traceability, coverage gate
68fe313a chore: apply cargo fmt
3aae8efb fix: validate manifest disk paths before local write/delete operations
b0a5a9a7 feat: S3 disk backup/restore support for ClickHouse 24.8+
d5ada8fd fix: round 4 full codebase audit (6-reviewer parallel review)
```

### src/restore/attach.rs (45.95%)
```
48765084 fix: restore resume idempotency, in-table parallelism, replica polling
50de942a fix: cargo fmt, graceful mutex poison, DRY tests, test-ID traceability, coverage gate
68fe313a chore: apply cargo fmt
06f7014d test: add 380+ unit tests to meet coverage gates
b0a5a9a7 feat: S3 disk backup/restore support for ClickHouse 24.8+
```

### src/upload/mod.rs (39.19%)
```
421cc296 fix: eliminate protobuf vulnerability, add coverage pipeline, wire part counters, DRY S3 disk helpers
50de942a fix: cargo fmt, graceful mutex poison, DRY tests, test-ID traceability, coverage gate
b0a5a9a7 feat: S3 disk backup/restore support for ClickHouse 24.8+
3cfa9906 fix: round 3 codebase audit fixes (prior session accumulated changes)
4bc7b5cb feat: thread CancellationToken into inner spawned tasks for kill propagation
```

## Branch Context

- **master** is the working branch (no separate main branch visible)
- All development is linear on master
- Conventional commit style: `feat:`, `fix:`, `refactor:`, `test:`, `chore:`, `docs:`
- Previous test commit used `test:` prefix: `caa30e4d test: add unit tests for pure functions and CI coverage tracking`

## Commit Style for This Plan

Based on recent history, the appropriate commit message style would be:
```
test: add unit tests for untested pure functions and raise CI coverage gate
```
