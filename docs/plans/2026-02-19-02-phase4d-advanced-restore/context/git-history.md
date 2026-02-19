# Git History Context

## Recent Repository History

```
bf2a7ddc style: apply cargo fmt to restore mod.rs
62b45a1b docs(restore): update CLAUDE.md for Phase 4c streaming engine postponement
ec926658 feat(restore): add Phase 2b execution block for postponed tables
ceeb65f3 feat(restore): populate postponed_tables in classify_restore_tables
eabb2009 feat(restore): add streaming engine and refreshable MV detection functions
e0668461 style: apply cargo fmt formatting
52e82d2d docs(backup,restore,clickhouse): update CLAUDE.md for Phase 4b dependency-aware restore
2cd7a8ea feat(restore): restructure restore() for phased architecture
259974a2 feat(restore): add create_functions() for Phase 4 function restoration
87e9bb69 feat(restore): add create_ddl_objects() with retry-loop fallback
4ae47472 feat(restore): add topo.rs with table classification and topological sort
6250dcbf feat(backup): populate TableManifest.dependencies from system.tables query
716a7364 feat(clickhouse): add query_table_dependencies() method to ChClient
ca7297a1 docs: Archive completed plan 2026-02-18-04-phase4a-table-remap
38248ec3 docs: Mark plan as COMPLETED
0eb22c67 style: apply cargo fmt formatting
68317e9b docs(restore,server): update CLAUDE.md for Phase 4a remap feature
b9e497b9 feat(server): pass remap parameters through restore and restore_remote API routes
f5120d91 feat(restore): wire CLI dispatch for --as, -m flags and restore_remote command
8fff26f1 feat(restore): integrate remap into restore pipeline
```

## File-Specific History

### src/restore/ (last 10 commits)

```
bf2a7ddc style: apply cargo fmt to restore mod.rs
62b45a1b docs(restore): update CLAUDE.md for Phase 4c streaming engine postponement
ec926658 feat(restore): add Phase 2b execution block for postponed tables
ceeb65f3 feat(restore): populate postponed_tables in classify_restore_tables
eabb2009 feat(restore): add streaming engine and refreshable MV detection functions
e0668461 style: apply cargo fmt formatting
52e82d2d docs(backup,restore,clickhouse): update CLAUDE.md for Phase 4b dependency-aware restore
2cd7a8ea feat(restore): restructure restore() for phased architecture
259974a2 feat(restore): add create_functions() for Phase 4 function restoration
87e9bb69 feat(restore): add create_ddl_objects() with retry-loop fallback
```

### src/clickhouse/client.rs (last 10 commits)

```
716a7364 feat(clickhouse): add query_table_dependencies() method to ChClient
e87552ef style: apply cargo fmt formatting
b933771c feat(clickhouse): add get_macros() method for system.macros query
816cc979 style: apply cargo fmt formatting to server module files
37666738 feat(clickhouse): add integration table DDL methods for API server
4300d5e4 style: apply cargo fmt formatting across all modules
9f4a80ba feat(clickhouse): add freeze_partition, query_system_parts, check_parts_columns, query_disk_free_space methods
bfa5a22b feat(clickhouse): wire TLS config fields into ChClient
1050619b style: apply cargo fmt formatting across all modules
b33a5463 feat(clickhouse): add remote_path field to DiskRow for S3 source resolution
```

## Branch Context

- **Current branch:** `master`
- **Commits ahead of main:** 0 (master and main are at the same point)
- **Working tree:** Modified files in `.claude/skills/self-healing/references/root-causes.md` (staged), plus build artifacts (unstaged)

## Phase Progression Context

The restore module has been incrementally enhanced through phases:
1. **Phase 4a** (remap): Added `remap.rs` with DDL rewriting, `RemapConfig`, `--as`/`-m` CLI flags
2. **Phase 4b** (dependencies): Added `topo.rs` with table classification, topological sort, phased architecture restructure
3. **Phase 4c** (streaming): Added streaming engine detection, refreshable MV detection, Phase 2b postponement
4. **Phase 4d** (this plan): Adds Mode A restore, ATTACH TABLE mode, ZK conflict resolution, ON CLUSTER, mutations

Each phase builds on the prior, and the phased architecture in `mod.rs` is already structured with clear phase boundaries (1 -> 2 -> 2b -> 3 -> 4) that Phase 4d can hook into for DROP ordering before CREATE.
