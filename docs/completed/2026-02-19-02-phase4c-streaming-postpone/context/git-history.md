# Git Context

## Recent Repository History

```
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
22215712 feat(restore): add remap module for --as and -m flag DDL rewriting
763422a7 docs: update CLAUDE.md for Phase 3e (docker/deploy)
b6703580 feat: add Dockerfile, CI workflow, integration tests, and K8s example
74b014ac feat(config): add WATCH_INTERVAL and FULL_INTERVAL env var overlay
e87552ef style: apply cargo fmt formatting
```

## File-Specific History

### src/restore/topo.rs
```
e0668461 style: apply cargo fmt formatting
4ae47472 feat(restore): add topo.rs with table classification and topological sort
```
Created in Phase 4b. Contains the placeholder `postponed_tables: Vec::new()` that Phase 4c fills.

### src/restore/mod.rs
```
2cd7a8ea feat(restore): restructure restore() for phased architecture
4ae47472 feat(restore): add topo.rs with table classification and topological sort
0eb22c67 style: apply cargo fmt formatting
8fff26f1 feat(restore): integrate remap into restore pipeline
22215712 feat(restore): add remap module for --as and -m flag DDL rewriting
4300d5e4 style: apply cargo fmt formatting across all modules
1e44ff68 feat(restore): add resume support with system.parts query
1050619b style: apply cargo fmt formatting across all modules
f345a175 feat(restore): add UUID-isolated S3 restore with same-name optimization
1d44f566 feat(restore): parallelize table restore with max_connections and engine-aware ATTACH
```
Heavily modified through Phases 2a-4b. The phased architecture (2cd7a8ea) is the most recent structural change.

### src/restore/schema.rs
```
259974a2 feat(restore): add create_functions() for Phase 4 function restoration
87e9bb69 feat(restore): add create_ddl_objects() with retry-loop fallback
0eb22c67 style: apply cargo fmt formatting
8fff26f1 feat(restore): integrate remap into restore pipeline
1050619b style: apply cargo fmt formatting across all modules
33414790 feat(restore): implement restore module with schema creation and part attachment
```

### src/table_filter.rs
```
4300d5e4 style: apply cargo fmt formatting across all modules
ff42860d feat(backup): add disk filtering via skip_disks and skip_disk_types
1050619b style: apply cargo fmt formatting across all modules
ffef293a feat(table_filter): add glob pattern matching for table selection
1bb4bc2e feat: add Phase 1 dependencies, error variants, and module declarations
```

## Branch Context

**Current branch:** master
**Commits ahead of main:** Could not determine (no main branch tracking, or master is the default)

## Relevant Prior Work

- **Phase 4b** (most recent): Added `topo.rs` with `RestorePhases`, `classify_restore_tables()`, `topological_sort()`. Restructured `restore()` for phased architecture. Created the `postponed_tables` placeholder.
- **Phase 4a**: Added remap module. The `create_tables()` and `create_ddl_objects()` functions already handle remap, so Phase 2b CREATE will get remap support for free.
- **Phase 2d**: Added resume state tracking. Postponed tables have no data parts, so resume state is not relevant for them.
