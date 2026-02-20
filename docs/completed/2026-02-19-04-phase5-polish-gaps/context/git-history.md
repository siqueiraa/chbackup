# Git Context

## Branch Information

- **Current branch:** `master`
- **Main branch:** `main`
- **Commits ahead of main:** 0 (master IS main, or same HEAD)

## Recent Repository History (last 20 commits)

```
9fff0b48 docs: update CLAUDE.md for Phase 4f operational extras
059e0929 feat(cli): implement tables command with live and remote modes
4a8474b4 feat(upload,download): wire compression format through pipelines
01f96c75 feat(compression): add multi-format compress_part and decompress_part
210ba7a0 feat(backup): integrate JSON column check into backup pre-flight
072af345 feat(list): add compressed size column to backup list output
ab3c364e feat(clickhouse): add check_json_columns() for Object/JSON type detection
2e04d783 chore: add zstd and flate2 compression dependencies
8053a9d9 docs: update CLAUDE.md for Phase 4e RBAC/config modules
7099bcb0 feat(cli): wire --rbac, --configs, --named-collections flags through all call sites
5395fb15 feat(restore): add RBAC, config, named collections restore and restart_command
3c31f3b0 feat(upload,download): add access/ and configs/ directory transfer
3e5cdf08 feat(backup): add RBAC, config, and named collections backup logic
328b2a69 feat(clickhouse): add RBAC, named collections, and UDF query methods
4ab7ada3 docs: Re-validate Phase 4e plan after PLAN.md updates
88005fb4 docs: Validate plan 2026-02-19-02-phase4e-rbac-config-backup (Phases 8-8.6)
6ea741b2 docs: Create plan 2026-02-19-02-phase4e-rbac-config-backup
467f4978 docs: Archive completed plan 2026-02-19-02-phase4d-advanced-restore
d5e176a3 docs: Mark plan as COMPLETED
05479a25 style: apply cargo fmt to Phase 4d source files
```

## File-Specific History (key files for this plan)

### src/server/routes.rs
```
059e0929 feat(cli): implement tables command with live and remote modes
7099bcb0 feat(cli): wire --rbac, --configs, --named-collections flags through all call sites
b9e497b9 feat(server): pass remap parameters through restore and restore_remote API routes
```

### src/main.rs
```
059e0929 feat(cli): implement tables command with live and remote modes
7099bcb0 feat(cli): wire --rbac, --configs, --named-collections flags through all call sites
ee753213 feat(restore): wire rm parameter through restore() and all call sites
f5120d91 feat(restore): wire CLI dispatch for --as, -m flags and restore_remote command
```

### src/error.rs
- No recent changes (stable since Phase 0)

### src/config.rs
- No recent changes to relevant fields

### src/list.rs
```
072af345 feat(list): add compressed size column to backup list output
```

### src/manifest.rs
- No recent changes (stable since Phase 2c added s3_objects)

## Relationship to This Plan

All 7 polish items are completing functionality that was deferred during earlier phases:
- Items 1-2 (API stubs): Created during Phase 3a (API server)
- Item 3 (skip-projections): CLI flag added during Phase 0 skeleton
- Item 4 (hardlink-exists-files): CLI flag added during Phase 0 skeleton
- Item 5 (progress bar): Config field added during Phase 0 skeleton
- Item 6 (exit codes): Design spec from doc section 11.6, never implemented
- Item 7 (list sizes): Response type created in Phase 3a with TODO comments

The codebase is in a stable state with all preceding phases complete. No merge conflicts expected.
