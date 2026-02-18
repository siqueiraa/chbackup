# Affected Modules Analysis

## Summary

- **Modules to update:** 6
- **Modules to create:** 0
- **Top-level files modified:** 5 (main.rs, list.rs, table_filter.rs, error.rs, possibly lib.rs)
- **Top-level files unchanged:** 3 (cli.rs, config.rs, manifest.rs -- all Phase 2d flags/fields already defined)
- **Git base:** 8d3b97b

## Modules Being Modified

| Module | CLAUDE.md Status | Triggers | Action | Summary of Changes |
|--------|------------------|----------|--------|---------------------|
| src/backup | EXISTS | new_patterns | UPDATE | Partition-level freeze, disk filtering, parts_columns check |
| src/clickhouse | EXISTS | new_patterns | UPDATE | TLS certs, system.parts query, free_space query, FREEZE PARTITION |
| src/download | EXISTS | new_patterns | UPDATE | Resume state, CRC64 verification+retry, disk space pre-flight |
| src/restore | EXISTS | new_patterns | UPDATE | Resume state, system.parts already-attached check |
| src/storage | EXISTS | (none) | UPDATE | Minor: manifest atomicity pattern |
| src/upload | EXISTS | new_patterns | UPDATE | Resume state, manifest atomic upload |

## Top-Level Files Modified

| File | Changes |
|------|---------|
| src/main.rs | Wire --resume, clean_broken, --partitions to implementations |
| src/list.rs | [BROKEN] display, clean_broken_local/remote impl |
| src/table_filter.rs | Add is_disk_excluded() |
| src/error.rs | Add ResumeError variant (optional) |

## Key Architecture Observations

1. **All CLI flags and config params already exist** -- Phase 2d was pre-scaffolded. The `--resume`, `--partitions`, `skip_disks`, `skip_disk_types`, `check_parts_columns`, TLS fields, and `use_resumable_state` are all defined but log warnings or are ignored.

2. **No new modules needed** -- All changes fit within existing module boundaries. Resume state files are a cross-cutting concern but don't warrant a new module (they can be simple serde JSON serialization alongside each command module).

3. **ChClient needs the most new methods** -- FREEZE PARTITION, system.parts query, system.disks free_space, system.parts_columns, and TLS certificate wiring.

4. **State degradation is a pattern, not a module** -- The `warn` on state file write failure pattern from design 16.1 applies everywhere state files are written.

## CLAUDE.md Update Tasks

1. **Update:** src/backup/CLAUDE.md -- document partition-level freeze, disk filtering, parts_columns check
2. **Update:** src/clickhouse/CLAUDE.md -- document new query methods, TLS cert handling
3. **Update:** src/download/CLAUDE.md -- document resume state, CRC64 verification, disk space pre-flight
4. **Update:** src/restore/CLAUDE.md -- document resume state, system.parts check
5. **Update:** src/upload/CLAUDE.md -- document resume state, manifest atomicity
6. **Update:** src/storage/CLAUDE.md -- document manifest atomicity pattern (minor)
