# Restore Guide

This guide covers all restore scenarios: full restore, partial restore, table renaming, database remapping, and troubleshooting.

## Table of contents

- [How restore works](#how-restore-works)
- [Basic restore](#basic-restore)
- [Restore from S3 (restore_remote)](#restore-from-s3-restore_remote)
- [Mode A: destructive restore (--rm)](#mode-a-destructive-restore---rm)
- [Mode B: non-destructive restore (default)](#mode-b-non-destructive-restore-default)
- [Table filtering](#table-filtering)
- [Table rename (--as)](#table-rename---as)
- [Database remap (-m)](#database-remap--m)
- [Partition filtering](#partition-filtering)
- [Schema only / Data only](#schema-only--data-only)
- [RBAC and config restore](#rbac-and-config-restore)
- [Resuming interrupted restores](#resuming-interrupted-restores)
- [Replicated tables](#replicated-tables)
- [ON CLUSTER restore](#on-cluster-restore)
- [Troubleshooting](#troubleshooting)

## How restore works

A restore follows these steps:

1. Read the backup manifest (`metadata.json`)
2. **Phase 0 (Mode A only)**: DROP existing tables and databases
3. Create databases (if they do not exist)
4. Create tables (DDL from the backup)
5. For each table, attach data parts:
   - **Local disk parts**: hardlink files from backup to `detached/` directory, then `ALTER TABLE ATTACH PART`
   - **S3 disk parts**: CopyObject in S3 to the table's UUID path, rewrite metadata, then `ALTER TABLE ATTACH PART`
6. Re-apply pending mutations (if any were captured during backup)
7. Fix file ownership (`chown` to ClickHouse user)

## Basic restore

Restore a local backup (already downloaded):

```bash
chbackup restore my-backup
```

This uses Mode B (non-destructive): it creates tables if they do not exist and attaches data. Existing data in existing tables is preserved.

## Restore from S3 (restore_remote)

Download and restore in one step:

```bash
chbackup restore_remote my-backup
```

This is equivalent to running `chbackup download my-backup` followed by `chbackup restore my-backup`.

Use `latest` to restore the most recent remote backup:

```bash
chbackup restore_remote latest
```

## Mode A: destructive restore (--rm)

Drops existing tables and databases before restoring. The restored state will match the backup exactly.

```bash
chbackup restore --rm my-backup
chbackup restore_remote --rm my-backup
```

What happens:

1. All tables in the backup are dropped (if they exist in ClickHouse)
2. Databases are dropped if they become empty
3. Databases and tables are recreated from backup DDL
4. Data parts are attached

System databases (`system`, `INFORMATION_SCHEMA`, `information_schema`) are never dropped.

Use Mode A when:

- You want to roll back to an exact point in time
- You are migrating to a new server and want a clean slate
- Schema has changed and you need to match the backup exactly

## Mode B: non-destructive restore (default)

The default mode. Does not drop anything. Creates databases and tables only if they do not exist, then attaches data parts.

```bash
chbackup restore my-backup
```

If a table already exists with data, the backup's data parts are attached alongside existing data. This is additive.

Use Mode B when:

- You want to add missing data without disrupting existing tables
- You are restoring a subset of tables
- You want to test a restore without risk

## Table filtering

Restore only specific tables using glob patterns:

```bash
# Restore all tables in the default database
chbackup restore -t "default.*" my-backup

# Restore a single table
chbackup restore -t "default.users" my-backup

# Restore multiple specific tables
chbackup restore -t "default.users,default.orders" my-backup

# Restore all tables matching a pattern
chbackup restore -t "*.events" my-backup
```

## Table rename (--as)

Restore tables under different names. Useful for testing or side-by-side comparison.

### Single table rename

Use `-t` to select the source table and `--as` to specify the destination:

```bash
chbackup restore -t default.users --as=default.users_restored my-backup
```

This restores `default.users` from the backup as `default.users_restored`.

### Multiple table renames

Use colon-separated pairs:

```bash
chbackup restore --as="default.users:default.users_copy,default.orders:default.orders_copy" my-backup
```

Each pair is `source_db.source_table:dest_db.dest_table`. You can rename to a different database:

```bash
chbackup restore --as="production.users:staging.users,production.orders:staging.orders" my-backup
```

## Database remap (-m)

Move all tables from one database to another:

```bash
chbackup restore -m "production:staging" my-backup
```

This restores every table from the `production` database into the `staging` database instead.

Remap multiple databases:

```bash
chbackup restore -m "production:staging,analytics:analytics_test" my-backup
```

Combine with table filter:

```bash
# Only restore tables from the production database, into staging
chbackup restore -t "production.*" -m "production:staging" my-backup
```

### Difference between --as and -m

- `--as` renames individual tables (one-to-one mapping)
- `-m` remaps entire databases (all tables in the source database go to the destination)

Use `--as` when you need fine-grained control over individual table names. Use `-m` when you want to move a whole database.

## Partition filtering

Restore only specific partitions:

```bash
# Restore only the January 2024 partition
chbackup restore --partitions="202401" my-backup

# Restore January and February
chbackup restore --partitions="202401,202402" my-backup
```

Partition IDs come from ClickHouse's partition expression. For monthly partitioning (`PARTITION BY toYYYYMM(ts)`), the IDs are like `202401`. For daily (`PARTITION BY toYYYYMMDD(ts)`), they are like `20240115`.

For unpartitioned tables (no PARTITION BY clause or `tuple()`), use `all`:

```bash
chbackup restore --partitions="all" my-backup
```

### Skip empty tables

When using `--partitions`, some tables may have no matching partitions. Use `--skip-empty-tables` to skip creating those tables:

```bash
chbackup restore --partitions="202401" --skip-empty-tables my-backup
```

Without this flag, tables with no matching partitions are still created (DDL only, no data).

## Schema only / Data only

### Schema only

Restore table definitions (DDL) without data. Useful for setting up a new cluster:

```bash
chbackup restore --schema my-backup
```

### Data only

Restore data parts without running any DDL. Tables must already exist with compatible schemas:

```bash
chbackup restore --data-only my-backup
```

These two flags cannot be used together.

### Workflow: schema first, then data

```bash
# Step 1: Create tables on the new cluster
chbackup restore --schema my-backup

# Step 2: Modify schemas if needed (add columns, change settings, etc.)

# Step 3: Attach data
chbackup restore --data-only my-backup
```

## RBAC and config restore

### RBAC objects

Restore users, roles, quotas, row policies, and settings profiles:

```bash
chbackup restore --rbac my-backup
```

If an object already exists, the behavior depends on `clickhouse.rbac_resolve_conflicts`:

- `recreate` (default): DROP and re-CREATE the object
- `ignore`: Skip existing objects
- `fail`: Return an error

### ClickHouse configuration

Restore server config files from the backup:

```bash
chbackup restore --configs my-backup
```

After restoring configs, ClickHouse needs a restart. chbackup runs the `clickhouse.restart_command` automatically.

### Named Collections

```bash
chbackup restore --named-collections my-backup
```

### All together

```bash
chbackup restore --rbac --configs --named-collections my-backup
```

## Resuming interrupted restores

If a restore is interrupted (crash, timeout, Ctrl+C), resume from where it left off:

```bash
chbackup restore --resume my-backup
```

Resume state is tracked per part. Parts that were already attached are skipped. The state file is stored alongside the backup data and is automatically deleted on completion.

Resume works for both `restore` and `restore_remote`:

```bash
chbackup restore_remote --resume my-backup
```

## Replicated tables

### ReplicatedMergeTree

chbackup handles Replicated tables automatically:

1. Before creating the table, it checks ZooKeeper for existing replicas
2. If a conflict is found (same replica name), it runs `SYSTEM DROP REPLICA` to clean up
3. Creates the table (which registers the new replica in ZK)
4. Attaches data parts

### ATTACH TABLE mode

For Replicated tables, an alternative restore mode uses DETACH/ATTACH TABLE instead of per-part ATTACH:

```yaml
clickhouse:
  restore_as_attach: true
```

This mode:

1. DETACH TABLE SYNC
2. DROP REPLICA in ZK
3. Hardlink all parts to the table's data directory
4. ATTACH TABLE
5. SYSTEM RESTORE REPLICA

This can be faster for tables with many parts. It falls back to normal per-part attach on failure. Note: ATTACH TABLE mode does not work for tables stored on S3 disks.

## ON CLUSTER restore

For ClickHouse clusters, execute DDL with ON CLUSTER:

```yaml
clickhouse:
  restore_schema_on_cluster: "my_cluster"
```

This adds `ON CLUSTER 'my_cluster'` to all CREATE DATABASE and CREATE TABLE statements. DDL is automatically skipped for DatabaseReplicated databases (they handle replication internally).

### Distributed table cluster rewrite

When restoring to a cluster with a different name:

```yaml
clickhouse:
  restore_distributed_cluster: "new_cluster_name"
```

This rewrites the cluster name in Distributed engine definitions during restore.

## Troubleshooting

### "Table already exists"

In Mode B (default), this is expected -- the table is skipped and data parts are attached to the existing table. If you want a clean restore, use `--rm`.

### "Part already attached" or duplicate data

If you restore the same backup twice in Mode B, you get duplicate data. Each run attaches new copies of the data parts. Use `--rm` for idempotent restores, or use `--resume` if the first restore was interrupted.

### "Cannot attach part: incompatible schema"

The table in ClickHouse has a different schema than the backup. Options:

1. Use `--rm` to drop and recreate the table with the backup's schema
2. Manually ALTER the table to match the backup's schema, then `--data-only`
3. Restore to a new table with `--as` and merge data manually

### Restore is slow

- Increase `clickhouse.max_connections` to restore more tables in parallel
- Check if S3 disk CopyObject is the bottleneck (increase `general.object_disk_server_side_copy_concurrency`)
- For tables with many small parts, the bottleneck is per-part ATTACH overhead

### "Replica already exists" ZooKeeper errors

chbackup attempts to resolve ZK conflicts automatically via `SYSTEM DROP REPLICA`. If this fails:

1. Check ZooKeeper connectivity from ClickHouse
2. Manually clean up the replica: `SYSTEM DROP REPLICA 'replica_name' FROM TABLE db.table`
3. Retry the restore

### Permissions errors after restore

chbackup runs `chown` to fix file ownership after restoring data. If you see permission errors:

1. Verify chbackup runs as root or the `clickhouse` user
2. Check that the ClickHouse user has read access to the data directory
3. In Docker, ensure the chbackup container runs with the same UID (101) as ClickHouse
