# CLI Gaps: Go clickhouse-backup vs Rust chbackup

Source comparison date: 2026-02-20

**Go source**: `Altinity/clickhouse-backup` @ `master`, file `cmd/clickhouse-backup/main.go`
**Rust source**: `src/cli.rs`

---

## 1. Global Flags

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--config, -c` | Default: `/etc/clickhouse-backup/config.yml`, env: `CLICKHOUSE_BACKUP_CONFIG` | Default: `/etc/chbackup/config.yml`, env: `CHBACKUP_CONFIG` | **Different default path and env var name.** Intentional rename for chbackup branding. Not a bug, but users migrating from Go need to know. |
| `--environment-override, --env` | `StringSliceFlag` (repeatable) | `--env` `Vec<String>` (repeatable) | OK. Go also accepts `--environment-override` long form; Rust only has `--env`. Minor alias difference. |
| `--command-id` | `IntFlag`, hidden, default `-1` | **MISSING** | **Missing.** Internal parameter used by the API server to track command IDs. Needed for API parity (server dispatches commands with an ID for status tracking). |

## 2. Command-Level Comparison

### 2.1 `tables`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--all, -a` | BoolFlag | `--all` (bool) | OK. Go has `-a` short alias; Rust does not. **Missing `-a` short alias.** |
| `--table, --tables, -t` | StringFlag | `--tables, -t` | OK. Go also accepts `--table` (singular); Rust only `--tables`. **Missing `--table` alias.** |
| `--remote-backup` | StringFlag | `--remote-backup` | OK. Match. |

### 2.2 `create`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--table, --tables, -t` | StringFlag | `-t, --tables` | OK. **Missing `--table` alias.** |
| `--diff-from-remote` | StringFlag | `--diff-from` (different name!) | **Name mismatch.** Go `create` has `--diff-from-remote` only (no `--diff-from`). Rust has `--diff-from` which maps to a local diff concept. The Go `create` command only supports remote diff base. See note below. |
| `--partitions` | StringSliceFlag (repeatable) | `Option<String>` (single value) | **Type mismatch.** Go accepts `--partitions` multiple times (e.g., `--partitions=db.t1:p1 --partitions=db.t2:p2`). Rust takes a single string. Multi-table partition specs require the repeatable form. |
| `--schema, -s` | BoolFlag | `--schema` (no `-s`) | **Missing `-s` short alias.** |
| `--rbac, --backup-rbac, --do-backup-rbac` | BoolFlag | `--rbac` | OK. **Missing `--backup-rbac` and `--do-backup-rbac` aliases.** |
| `--configs, --backup-configs, --do-backup-configs` | BoolFlag | `--configs` | OK. **Missing `--backup-configs` and `--do-backup-configs` aliases.** |
| `--named-collections, --backup-named-collections, --do-backup-named-collections` | BoolFlag | `--named-collections` | OK. **Missing longer aliases.** |
| `--rbac-only` | BoolFlag | **MISSING** | **Missing flag.** Allows backing up RBAC objects exclusively (skips data). |
| `--configs-only` | BoolFlag | **MISSING** | **Missing flag.** Allows backing up server configs exclusively. |
| `--named-collections-only` | BoolFlag | **MISSING** | **Missing flag.** Allows backing up named collections exclusively. |
| `--skip-check-parts-columns` | BoolFlag | `--skip-check-parts-columns` | OK. Match. |
| `--skip-projections` | StringSliceFlag (repeatable) | `Option<String>` (single value) | **Type mismatch.** Go accepts multiple `--skip-projections` values. Rust takes single comma-separated string. |
| `--resume, --resumable` | BoolFlag | `--resume` | OK. **Missing `--resumable` alias.** |
| backup_name | Positional arg (first) | `Option<String>` positional | OK. Match. |

**Note on `--diff-from` vs `--diff-from-remote`**: In Go, the `create` command ONLY has `--diff-from-remote` (remote base). The `--diff-from` flag (local base) exists only on `create_remote` and `upload`. The Rust `create` command has `--diff-from` which is semantically ambiguous -- it should be `--diff-from-remote` to match Go.

### 2.3 `create_remote`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--table, --tables, -t` | StringFlag | `-t, --tables` | OK. **Missing `--table` alias.** |
| `--partitions` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** Go supports partition filtering on create_remote. |
| `--diff-from` | StringFlag | **MISSING** | **Missing flag.** Go `create_remote` supports local incremental base via `--diff-from`. |
| `--diff-from-remote` | StringFlag | `--diff-from-remote` | OK. Match. |
| `--schema, -s` | BoolFlag | **MISSING** | **Missing flag.** Go supports schema-only create_remote. |
| `--rbac, --backup-rbac, --do-backup-rbac` | BoolFlag | `--rbac` | OK. **Missing longer aliases.** |
| `--configs, --backup-configs, --do-backup-configs` | BoolFlag | `--configs` | OK. **Missing longer aliases.** |
| `--named-collections, --backup-named-collections, --do-backup-named-collections` | BoolFlag | `--named-collections` | OK. **Missing longer aliases.** |
| `--rbac-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--configs-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--named-collections-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--resume, --resumable` | BoolFlag | `--resume` | OK. **Missing `--resumable` alias.** |
| `--skip-check-parts-columns` | BoolFlag | `--skip-check-parts-columns` | OK. Match. |
| `--skip-projections` | StringSliceFlag (repeatable) | `--skip-projections` (single value) | **Type mismatch** (single vs repeatable). |
| `--delete, --delete-source, --delete-local` | BoolFlag | `--delete-source` | OK. **Missing `--delete` and `--delete-local` aliases.** |
| backup_name | Positional (first) | `Option<String>` | OK. Match. |

### 2.4 `upload`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--diff-from` | StringFlag | **MISSING** | **Missing flag.** Go upload supports local incremental base. |
| `--diff-from-remote` | StringFlag | `--diff-from-remote` | OK. Match. |
| `--table, --tables, -t` | StringFlag | **MISSING** | **Missing flag.** Go upload supports table filtering. |
| `--partitions` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** Go upload supports partition filtering. |
| `--schema, -s` | BoolFlag | **MISSING** | **Missing flag.** Go upload supports schema-only upload. |
| `--rbac-only, --rbac` | BoolFlag | **MISSING** | **Missing flag.** |
| `--configs-only, --configs` | BoolFlag | **MISSING** | **Missing flag.** |
| `--named-collections-only, --named-collections` | BoolFlag | **MISSING** | **Missing flag.** |
| `--skip-projections` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** |
| `--resume, --resumable` | BoolFlag | `--resume` | OK. **Missing `--resumable` alias.** |
| `--delete, --delete-source, --delete-local` | BoolFlag | `--delete-local` | OK. **Missing `--delete` and `--delete-source` aliases.** The Rust flag is `--delete-local`; Go primary is `--delete`. |
| backup_name | Positional (first) | `Option<String>` | OK. Match. |

### 2.5 `download`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--table, --tables, -t` | StringFlag | **MISSING** | **Missing flag.** Go download supports table filtering. |
| `--partitions` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** Go download supports partition filtering. |
| `--schema, --schema-only, -s` | BoolFlag | **MISSING** | **Missing flag.** Go download supports schema-only download. |
| `--rbac-only, --rbac` | BoolFlag | **MISSING** | **Missing flag.** |
| `--configs-only, --configs` | BoolFlag | **MISSING** | **Missing flag.** |
| `--named-collections-only, --named-collections` | BoolFlag | **MISSING** | **Missing flag.** |
| `--resume, --resumable` | BoolFlag | `--resume` | OK. **Missing `--resumable` alias.** |
| `--hardlink-exists-files` | BoolFlag | `--hardlink-exists-files` | OK. Match. |
| backup_name | Positional (first) | `Option<String>` | OK. Match. |

### 2.6 `restore`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--table, --tables, -t` | StringFlag | `-t, --tables` | OK. **Missing `--table` alias.** |
| `--restore-database-mapping, -m` | StringSliceFlag (repeatable) | `-m, --database-mapping` (single string) | **Name mismatch + type mismatch.** Go uses `--restore-database-mapping`; Rust uses `--database-mapping`. Go supports repeatable flag for multiple mappings. |
| `--restore-table-mapping, --tm` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** Go supports table-level remapping (not just database). |
| `--partitions` | StringSliceFlag (repeatable) | `Option<String>` (single value) | **Type mismatch** (single vs repeatable). |
| `--schema, -s` | BoolFlag | `--schema` (no `-s`) | **Missing `-s` short alias.** |
| `--data, -d` | BoolFlag | `--data-only` (different name, no `-d` short) | **Name mismatch.** Go: `--data, -d`. Rust: `--data-only`. Also **missing `-d` short alias.** |
| `--rm, --drop` | BoolFlag | `--rm` with visible_alias `drop` | OK. Match. |
| `--i, --ignore-dependencies` | BoolFlag | **MISSING** | **Missing flag.** Used to ignore dependencies when dropping objects. |
| `--rbac, --restore-rbac, --do-restore-rbac` | BoolFlag | `--rbac` | OK. **Missing `--restore-rbac` and `--do-restore-rbac` aliases.** |
| `--configs, --restore-configs, --do-restore-configs` | BoolFlag | `--configs` | OK. **Missing longer aliases.** |
| `--named-collections, --restore-named-collections, --do-restore-named-collections` | BoolFlag | `--named-collections` | OK. **Missing longer aliases.** |
| `--rbac-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--configs-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--named-collections-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--skip-projections` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** Go restore supports skipping projections. |
| `--resume, --resumable` | BoolFlag | `--resume` | OK. **Missing `--resumable` alias.** |
| `--restore-schema-as-attach` | BoolFlag | **MISSING** | **Missing CLI flag.** (May be config-only in Rust via `restore_as_attach`). Go exposes it as a CLI flag. |
| `--replicated-copy-to-detached` | BoolFlag | **MISSING** | **Missing flag.** Copies data to detached folder for Replicated tables but skips ATTACH PART. |
| `--skip-empty-tables` | BoolFlag | `--skip-empty-tables` | OK. Match. |
| `--as` (rename) | Not in Go | Present in Rust | **Rust-only flag.** This is per our design doc. Not a gap. |
| backup_name | Positional (first) | `Option<String>` | OK. Match. |

### 2.7 `restore_remote`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--table, --tables, -t` | StringFlag | `-t, --tables` | OK. **Missing `--table` alias.** |
| `--restore-database-mapping, -m` | StringSliceFlag (repeatable) | `-m, --database-mapping` (single string) | **Name mismatch + type mismatch.** Same as restore. |
| `--restore-table-mapping, --tm` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** |
| `--partitions` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** Go restore_remote supports partition filtering. |
| `--schema, -s` | BoolFlag | **MISSING** | **Missing flag.** |
| `--data, -d` | BoolFlag | **MISSING** | **Missing flag.** |
| `--rm, --drop` | BoolFlag | `--rm` with visible_alias `drop` | OK. Match. |
| `--i, --ignore-dependencies` | BoolFlag | **MISSING** | **Missing flag.** |
| `--rbac, --restore-rbac, --do-restore-rbac` | BoolFlag | `--rbac` | OK. **Missing longer aliases.** |
| `--configs, --restore-configs, --do-restore-configs` | BoolFlag | `--configs` | OK. **Missing longer aliases.** |
| `--named-collections, --restore-named-collections, --do-restore-named-collections` | BoolFlag | `--named-collections` | OK. **Missing longer aliases.** |
| `--rbac-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--configs-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--named-collections-only` | BoolFlag | **MISSING** | **Missing flag.** |
| `--skip-projections` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** |
| `--resume, --resumable` | BoolFlag | `--resume` | OK. **Missing `--resumable` alias.** |
| `--restore-schema-as-attach` | BoolFlag | **MISSING** | **Missing flag.** |
| `--replicated-copy-to-detached` | BoolFlag | **MISSING** | **Missing flag.** |
| `--hardlink-exists-files` | BoolFlag | **MISSING** | **Missing flag.** Go restore_remote supports hardlink dedup. |
| `--skip-empty-tables` | BoolFlag | `--skip-empty-tables` | OK. Match. |
| backup_name | Positional (first) | `Option<String>` | OK. Match. |

### 2.8 `list`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--format, -f` | StringFlag (text\|json\|yaml\|csv\|tsv) | `--format` ValueEnum (Default\|Json\|Yaml\|Csv\|Tsv) | **Missing `-f` short alias.** Also the Rust default variant is `Default` instead of empty/text. |
| Positional 1 | `all\|local\|remote` (string) | `Option<Location>` (Local\|Remote) | **Missing `all` variant.** Go accepts `all` as explicit option. Rust treats `None` as "all" which is functionally equivalent, but `all` as an explicit positional is not accepted. |
| Positional 2 | `latest\|previous` (string) | **MISSING as positional** | **Missing `latest`/`previous` positional shortcuts.** These are mentioned in CLAUDE.md as implemented in list.rs, but the CLI definition does not expose them as positional arguments. |

### 2.9 `delete`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| Positional 1 | `local\|remote` | `Location` (enum) | OK. Match. |
| Positional 2 | backup name (required in Go) | `Option<String>` (optional in Rust) | **Rust makes it optional.** Go explicitly validates it's non-empty and errors. Rust should require it. |

### 2.10 `clean`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| (no flags) | No additional flags | `--name` (optional) | **Rust-only flag.** Rust has an optional `--name` to target a specific backup. Go cleans everything. Not a gap per se -- Rust is a superset. |

### 2.11 `clean_remote_broken` / `clean_local_broken`

Go has **two separate commands**: `clean_remote_broken` and `clean_local_broken`.
Rust has **one combined command**: `clean_broken` with a `location` argument (Local|Remote).

| Aspect | Go | Rust | Gap |
|--------|----|------|-----|
| Command name(s) | `clean_remote_broken`, `clean_local_broken` | `clean_broken <local\|remote>` | **Different command structure.** Rust unified approach is arguably better, but breaks Go CLI compatibility for scripts using `clean_remote_broken` or `clean_local_broken` directly. |

### 2.12 `watch`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--watch-interval` | StringFlag | `--watch-interval` | OK. Match. |
| `--full-interval` | StringFlag | `--full-interval` | OK. Match. |
| `--watch-backup-name-template` | StringFlag | `--name-template` | **Name mismatch.** Go uses `--watch-backup-name-template`; Rust uses `--name-template`. |
| `--table, --tables, -t` | StringFlag | `-t, --tables` | OK. **Missing `--table` alias.** |
| `--partitions` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** Go watch supports partition filtering. |
| `--schema, -s` | BoolFlag | **MISSING** | **Missing flag.** Go watch supports schema-only mode. |
| `--rbac, --backup-rbac, --do-backup-rbac` | BoolFlag | **MISSING** | **Missing flag.** |
| `--configs, --backup-configs, --do-backup-configs` | BoolFlag | **MISSING** | **Missing flag.** |
| `--named-collections, --backup-named-collections, --do-backup-named-collections` | BoolFlag | **MISSING** | **Missing flag.** |
| `--skip-check-parts-columns` | BoolFlag | **MISSING** | **Missing flag.** |
| `--skip-projections` | StringSliceFlag (repeatable) | **MISSING** | **Missing flag.** |
| `--delete, --delete-source, --delete-local` | BoolFlag | **MISSING** | **Missing flag.** Explicitly delete local backup during upload in watch mode. |

### 2.13 `server`

| Flag | Go | Rust | Gap |
|------|----|------|-----|
| `--watch` | BoolFlag | `--watch` | OK. Match. |
| `--watch-interval` | StringFlag | **MISSING** | **Missing flag.** Go server accepts watch interval override. |
| `--full-interval` | StringFlag | **MISSING** | **Missing flag.** |
| `--watch-backup-name-template` | StringFlag | **MISSING** | **Missing flag.** |
| `--watch-delete-source, --watch-delete-local` | BoolFlag | **MISSING** | **Missing flag.** Delete local backups in watch mode via server flag. |

### 2.14 `default-config` and `print-config`

Both commands match -- no flags beyond global flags. OK.

---

## 3. Summary of Gaps by Severity

### 3.1 CRITICAL -- Functional gaps that break user workflows

| # | Gap | Commands Affected |
|---|-----|-------------------|
| 1 | `--partitions` is `Option<String>` instead of repeatable `Vec<String>` | create, restore (and missing entirely from create_remote, upload, download, restore_remote, watch) |
| 2 | `--restore-table-mapping, --tm` flag entirely missing | restore, restore_remote |
| 3 | `--diff-from` on `upload` missing (local incremental base) | upload |
| 4 | `--diff-from` on `create_remote` missing (local incremental base) | create_remote |
| 5 | `--table, --tables, -t` missing from `upload` and `download` | upload, download |
| 6 | `--schema, -s` missing from `upload`, `download`, `create_remote`, `restore_remote` | upload, download, create_remote, restore_remote |
| 7 | `--data, -d` missing from `restore_remote` | restore_remote |
| 8 | `--ignore-dependencies, -i` missing from `restore` and `restore_remote` | restore, restore_remote |
| 9 | `--command-id` hidden global flag missing (needed for API server integration) | all commands |
| 10 | `--restore-schema-as-attach` missing as CLI flag | restore, restore_remote |
| 11 | `--replicated-copy-to-detached` missing | restore, restore_remote |
| 12 | `--hardlink-exists-files` missing from `restore_remote` | restore_remote |

### 3.2 MODERATE -- Missing *-only flags and aliases

| # | Gap | Commands Affected |
|---|-----|-------------------|
| 13 | `--rbac-only`, `--configs-only`, `--named-collections-only` missing | create, create_remote, upload, download, restore, restore_remote |
| 14 | `--skip-projections` missing from upload, restore, restore_remote | upload, restore, restore_remote |
| 15 | Various missing flag aliases (`--resumable`, `--table` singular, `-s`, `-d`, `-a`, `-f`) | multiple |
| 16 | `--restore-database-mapping` name mismatch (Rust: `--database-mapping`) | restore, restore_remote |
| 17 | `--data-only` name (Rust) vs `--data` (Go) on restore | restore |

### 3.3 LOW -- Structural/naming differences (may be intentional)

| # | Gap | Note |
|---|-----|------|
| 18 | `clean_remote_broken` / `clean_local_broken` vs `clean_broken <location>` | Rust unified approach. May want aliases for Go compat. |
| 19 | `--watch-backup-name-template` (Go) vs `--name-template` (Rust) | Naming difference on watch command. |
| 20 | Config path default: `/etc/clickhouse-backup/config.yml` (Go) vs `/etc/chbackup/config.yml` (Rust) | Intentional rebrand. |
| 21 | Env var: `CLICKHOUSE_BACKUP_CONFIG` (Go) vs `CHBACKUP_CONFIG` (Rust) | Intentional rebrand. |
| 22 | `list` missing `all` as explicit positional; missing `latest`/`previous` positional args in CLI def | Functional in code but not in CLI arg definition. |
| 23 | `delete` backup_name is optional in Rust but required in Go | Should be required. |
| 24 | Watch command missing many pass-through flags (--partitions, --schema, --rbac, etc.) | These get passed through to create_remote internally. |
| 25 | Server command missing watch-related flag overrides | --watch-interval, --full-interval, --watch-backup-name-template, --watch-delete-source. |

---

## 4. Flag Alias Inventory

The Go CLI uses urfave/cli which allows comma-separated alias definitions. Many Go flags have 2-3 aliases for backward compatibility. Below is a full inventory of aliases present in Go but missing in Rust.

| Go Flag Aliases | Rust Has | Missing Aliases |
|----------------|----------|-----------------|
| `--table, --tables, -t` | `--tables, -t` | `--table` |
| `--all, -a` (tables) | `--all` | `-a` |
| `--schema, -s` | `--schema` | `-s` |
| `--data, -d` | `--data-only` | `--data`, `-d` |
| `--rm, --drop` | `--rm` (alias `drop`) | OK |
| `--i, --ignore-dependencies` | N/A | Both missing |
| `--resume, --resumable` | `--resume` | `--resumable` |
| `--rbac, --backup-rbac, --do-backup-rbac` | `--rbac` | `--backup-rbac`, `--do-backup-rbac` |
| `--rbac, --restore-rbac, --do-restore-rbac` | `--rbac` | `--restore-rbac`, `--do-restore-rbac` |
| `--configs, --backup-configs, --do-backup-configs` | `--configs` | `--backup-configs`, `--do-backup-configs` |
| `--configs, --restore-configs, --do-restore-configs` | `--configs` | `--restore-configs`, `--do-restore-configs` |
| `--named-collections, --backup-named-collections, --do-backup-named-collections` | `--named-collections` | `--backup-named-collections`, `--do-backup-named-collections` |
| `--named-collections, --restore-named-collections, --do-restore-named-collections` | `--named-collections` | `--restore-named-collections`, `--do-restore-named-collections` |
| `--delete, --delete-source, --delete-local` | `--delete-source` (create_remote), `--delete-local` (upload) | Inconsistent naming; Go uses all three as aliases on same flag. |
| `--restore-database-mapping, -m` | `--database-mapping, -m` | `--restore-database-mapping` |
| `--restore-table-mapping, --tm` | N/A | Entirely missing |
| `--format, -f` (list) | `--format` | `-f` |
| `--schema, --schema-only, -s` (download) | N/A | All missing from download |
| `--watch-backup-name-template` (watch/server) | `--name-template` | `--watch-backup-name-template` |
| `--watch-delete-source, --watch-delete-local` (server) | N/A | Both missing |

---

## 5. `--partitions` Type Difference (Detail)

This is worth calling out explicitly because it affects multiple commands.

**Go behavior**: `--partitions` is a `StringSliceFlag`, meaning users can specify it multiple times:
```
clickhouse-backup create --partitions=db.table1:part1,part2 --partitions=db.table2:part3 my_backup
```
Each `--partitions` invocation adds to a slice. This is essential for the per-table partition syntax (`db.table:partition`).

**Rust behavior**: `--partitions` is `Option<String>`, a single string value. Users would need to cram everything into one comma-separated value, which conflicts with the per-table syntax that uses commas internally.

**Fix needed**: Change to `Vec<String>` with `#[arg(long, action = clap::ArgAction::Append)]` or similar.

---

## 6. Commands Present in Rust but Not in Go

| Rust Command | Note |
|-------------|------|
| `--as` flag on restore/restore_remote | Rust-specific single-table rename. Go uses `--restore-table-mapping` instead. |
| `--name` flag on `clean` | Rust-specific targeted shadow cleanup. Go cleans all shadows. |
| `clean_broken` (unified) | Replaces Go's separate `clean_remote_broken` / `clean_local_broken`. |

These are Rust enhancements, not gaps. However, for drop-in compatibility, consider adding `clean_remote_broken` and `clean_local_broken` as command aliases.

---

## 7. Recommended Priority Order for Fixes

1. **Make `--partitions` repeatable** (`Vec<String>`) across all commands that have it
2. **Add missing flags to `upload`**: `--tables`, `--diff-from`, `--partitions`, `--schema`, `--skip-projections`, `--delete` alias
3. **Add missing flags to `download`**: `--tables`, `--partitions`, `--schema`, RBAC/configs/named-collections-only
4. **Add `--restore-table-mapping`** to restore and restore_remote
5. **Add `--ignore-dependencies`** to restore and restore_remote
6. **Add missing flags to `create_remote`**: `--partitions`, `--diff-from`, `--schema`
7. **Add missing flags to `restore_remote`**: `--partitions`, `--schema`, `--data`, `--skip-projections`, `--hardlink-exists-files`, `--restore-schema-as-attach`, `--replicated-copy-to-detached`
8. **Add `*-only` flags** (`--rbac-only`, `--configs-only`, `--named-collections-only`) to all relevant commands
9. **Add `--restore-schema-as-attach` and `--replicated-copy-to-detached`** to restore
10. **Add watch pass-through flags**: `--partitions`, `--schema`, `--rbac`, `--configs`, `--named-collections`, `--skip-check-parts-columns`, `--skip-projections`, `--delete`
11. **Add server watch flags**: `--watch-interval`, `--full-interval`, `--watch-backup-name-template`, `--watch-delete-source`
12. **Add `--command-id`** hidden global flag
13. **Fix flag naming**: `--database-mapping` -> `--restore-database-mapping`, `--data-only` -> `--data`, `--name-template` -> `--watch-backup-name-template`
14. **Add all missing short aliases**: `-s`, `-d`, `-a`, `-f`, `-i`
15. **Add backward-compat long aliases** for rbac/configs/named-collections variants
16. **Add `latest`/`previous` as positional args** to list command definition
17. **Make `delete` backup_name required** (not optional)
