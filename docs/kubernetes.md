# Kubernetes Deployment Guide

chbackup runs as a sidecar container alongside ClickHouse, sharing the same data volume. This guide covers the sidecar pattern, configuration, monitoring, and common deployment scenarios.

A ready-to-use sidecar manifest is in [`examples/kubernetes/sidecar.yaml`](../examples/kubernetes/sidecar.yaml).

## Table of contents

- [How it works](#how-it-works)
- [Prerequisites](#prerequisites)
- [Basic sidecar deployment](#basic-sidecar-deployment)
- [Secrets management](#secrets-management)
- [Health checks](#health-checks)
- [Prometheus monitoring](#prometheus-monitoring)
- [Watch mode (scheduled backups)](#watch-mode-scheduled-backups)
- [On-demand operations via API](#on-demand-operations-via-api)
- [StatefulSet deployment](#statefulset-deployment)
- [Resource sizing](#resource-sizing)
- [Graceful shutdown](#graceful-shutdown)
- [Multiple shards](#multiple-shards)
- [Migrating from Go clickhouse-backup](#migrating-from-go-clickhouse-backup)
- [Troubleshooting](#troubleshooting)

## How it works

chbackup needs direct filesystem access to `/var/lib/clickhouse/` because ClickHouse FREEZE creates hardlinks in the data directory. This means chbackup must run in the same pod as ClickHouse, sharing the data volume.

The typical setup is:

1. ClickHouse runs as the main container
2. chbackup runs as a sidecar in `server --watch` mode
3. Both containers mount the same volume at `/var/lib/clickhouse`
4. chbackup exposes port 7171 for the HTTP API and Prometheus metrics

## Prerequisites

1. A running Kubernetes cluster
2. An S3 bucket (or S3-compatible storage) for storing backups
3. S3 credentials stored as a Kubernetes Secret

Create the credentials secret:

```bash
kubectl create secret generic chbackup-s3-creds \
  --from-literal=access-key=YOUR_ACCESS_KEY \
  --from-literal=secret-key=YOUR_SECRET_KEY
```

If you use IRSA (EKS) or Workload Identity (GKE with S3), you can skip the secret and let the SDK pick up credentials from the service account.

## Basic sidecar deployment

This is a minimal Deployment that runs ClickHouse with a chbackup sidecar. Both containers share the data volume.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: clickhouse
  labels:
    app: clickhouse
spec:
  replicas: 1
  selector:
    matchLabels:
      app: clickhouse
  template:
    metadata:
      labels:
        app: clickhouse
    spec:
      containers:
        # ClickHouse server
        - name: clickhouse
          image: clickhouse/clickhouse-server:24.8
          ports:
            - name: http
              containerPort: 8123
            - name: native
              containerPort: 9000
          volumeMounts:
            - name: data
              mountPath: /var/lib/clickhouse
          readinessProbe:
            httpGet:
              path: /ping
              port: http
            initialDelaySeconds: 5
            periodSeconds: 10

        # chbackup sidecar
        - name: chbackup
          image: ghcr.io/user/chbackup:latest
          args: ["server", "--watch"]
          ports:
            - name: api
              containerPort: 7171
          env:
            - name: S3_BUCKET
              value: "my-clickhouse-backups"
            - name: S3_REGION
              value: "us-east-1"
            - name: S3_ACCESS_KEY
              valueFrom:
                secretKeyRef:
                  name: chbackup-s3-creds
                  key: access-key
            - name: S3_SECRET_KEY
              valueFrom:
                secretKeyRef:
                  name: chbackup-s3-creds
                  key: secret-key
            - name: WATCH_INTERVAL
              value: "1h"
            - name: FULL_INTERVAL
              value: "24h"
          volumeMounts:
            - name: data
              mountPath: /var/lib/clickhouse
          readinessProbe:
            httpGet:
              path: /api/v1/status
              port: api
            initialDelaySeconds: 5
            periodSeconds: 10

      volumes:
        - name: data
          emptyDir: {}
```

Save this as `clickhouse-deployment.yaml` and apply:

```bash
kubectl apply -f clickhouse-deployment.yaml
```

Verify both containers are running:

```bash
kubectl get pods -l app=clickhouse
kubectl logs -l app=clickhouse -c chbackup --tail=20
```

## Secrets management

### Option 1: Kubernetes Secret (shown above)

```bash
kubectl create secret generic chbackup-s3-creds \
  --from-literal=access-key=AKIAIOSFODNN7EXAMPLE \
  --from-literal=secret-key=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
```

Reference in the pod spec with `secretKeyRef`.

### Option 2: IRSA (EKS)

Create an IAM role with S3 permissions and annotate the service account:

```yaml
apiVersion: v1
kind: ServiceAccount
metadata:
  name: chbackup
  annotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123456789012:role/chbackup-s3-role
```

Add `serviceAccountName: chbackup` to the pod spec. No S3 credential env vars needed.

### Option 3: Config file as a ConfigMap

Mount a full config file instead of using env vars:

```bash
kubectl create configmap chbackup-config \
  --from-file=config.yml=my-chbackup-config.yml
```

```yaml
- name: chbackup
  image: ghcr.io/user/chbackup:latest
  args: ["server", "--watch", "-c", "/etc/chbackup/config.yml"]
  volumeMounts:
    - name: data
      mountPath: /var/lib/clickhouse
    - name: config
      mountPath: /etc/chbackup
      readOnly: true

volumes:
  - name: config
    configMap:
      name: chbackup-config
```

You can combine a ConfigMap for non-sensitive values with a Secret for credentials via env vars. Environment variables override config file values.

## Health checks

chbackup provides two endpoints for Kubernetes probes:

```yaml
readinessProbe:
  httpGet:
    path: /api/v1/status
    port: 7171
  initialDelaySeconds: 5
  periodSeconds: 10

livenessProbe:
  httpGet:
    path: /health
    port: 7171
  initialDelaySeconds: 10
  periodSeconds: 30
```

- `/health` returns `{"status":"ok"}` if the HTTP server is alive. Use for liveness.
- `/api/v1/status` returns server status including ClickHouse connection state. Use for readiness.

If you have Basic auth enabled (`api.username` / `api.password`), the probes need auth headers or you need to configure the probes to pass. An alternative is to use a TCP probe on port 7171.

## Prometheus monitoring

chbackup exposes Prometheus metrics at `/metrics` on port 7171.

### Pod annotations

```yaml
metadata:
  annotations:
    prometheus.io/scrape: "true"
    prometheus.io/port: "7171"
    prometheus.io/path: "/metrics"
```

### ServiceMonitor (Prometheus Operator)

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: chbackup
  labels:
    release: prometheus
spec:
  selector:
    matchLabels:
      app: clickhouse
  endpoints:
    - port: api
      path: /metrics
      interval: 30s
```

You need a Service that exposes the chbackup port:

```yaml
apiVersion: v1
kind: Service
metadata:
  name: clickhouse
  labels:
    app: clickhouse
spec:
  selector:
    app: clickhouse
  ports:
    - name: http
      port: 8123
    - name: native
      port: 9000
    - name: api
      port: 7171
```

### Key metrics

| Metric | Type | Description |
|--------|------|-------------|
| `chbackup_operations_total` | Counter | Total operations by command and status |
| `chbackup_operation_duration_seconds` | Histogram | Operation duration by command |
| `chbackup_watch_state` | Gauge | Current watch loop state (1-7) |
| `chbackup_watch_last_full_timestamp` | Gauge | Timestamp of last full backup |
| `chbackup_watch_last_incremental_timestamp` | Gauge | Timestamp of last incremental backup |
| `chbackup_watch_consecutive_errors` | Gauge | Current consecutive error count |

## Watch mode (scheduled backups)

With `--watch`, chbackup runs a continuous loop:

1. Create a backup (full or incremental based on schedule)
2. Upload to S3
3. Delete local backup (if configured)
4. Run retention cleanup
5. Sleep until next interval

Configure via env vars (most common in K8s):

```yaml
env:
  - name: WATCH_INTERVAL
    value: "1h"        # check every hour
  - name: FULL_INTERVAL
    value: "24h"       # full backup every 24 hours
```

Or via config file:

```yaml
watch:
  watch_interval: 1h
  full_interval: 24h
  name_template: "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}"
  max_consecutive_errors: 5
  delete_local_after_upload: true
```

Watch mode resumes after pod restarts. It scans remote backups matching the name template to determine when the last full and incremental backups were made.

## On-demand operations via API

Trigger backup and restore operations from outside the pod:

```bash
# Port-forward to the chbackup API
kubectl port-forward pod/clickhouse-xyz 7171:7171

# Create a backup
curl -X POST http://localhost:7171/api/v1/create

# Upload the latest backup
curl -X POST http://localhost:7171/api/v1/upload/latest

# List backups
curl http://localhost:7171/api/v1/list

# Download and restore
curl -X POST http://localhost:7171/api/v1/restore_remote/my-backup

# Check watch status
curl http://localhost:7171/api/v1/watch/status
```

See the [API documentation](api.md) for the complete endpoint reference.

## StatefulSet deployment

For ClickHouse clusters using StatefulSets with persistent volumes:

```yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: clickhouse
spec:
  serviceName: clickhouse
  replicas: 3
  selector:
    matchLabels:
      app: clickhouse
  template:
    metadata:
      labels:
        app: clickhouse
    spec:
      containers:
        - name: clickhouse
          image: clickhouse/clickhouse-server:24.8
          volumeMounts:
            - name: data
              mountPath: /var/lib/clickhouse

        - name: chbackup
          image: ghcr.io/user/chbackup:latest
          args: ["server", "--watch"]
          env:
            - name: S3_BUCKET
              value: "my-clickhouse-backups"
            - name: S3_PREFIX
              value: "chbackup/{shard}"
            - name: S3_ACCESS_KEY
              valueFrom:
                secretKeyRef:
                  name: chbackup-s3-creds
                  key: access-key
            - name: S3_SECRET_KEY
              valueFrom:
                secretKeyRef:
                  name: chbackup-s3-creds
                  key: secret-key
            - name: WATCH_INTERVAL
              value: "1h"
            - name: FULL_INTERVAL
              value: "24h"
          volumeMounts:
            - name: data
              mountPath: /var/lib/clickhouse

  volumeClaimTemplates:
    - metadata:
        name: data
      spec:
        accessModes: ["ReadWriteOnce"]
        resources:
          requests:
            storage: 100Gi
```

The `{shard}` macro in `S3_PREFIX` is resolved from ClickHouse `system.macros`, so each replica writes to a different S3 prefix. This prevents backup name collisions across shards.

## Resource sizing

Recommended starting points:

| Workload size | CPU request | CPU limit | Memory request | Memory limit |
|---------------|------------|-----------|----------------|--------------|
| Small (< 50 GB) | 100m | 500m | 128 Mi | 256 Mi |
| Medium (50-500 GB) | 250m | 1000m | 256 Mi | 512 Mi |
| Large (> 500 GB) | 500m | 2000m | 512 Mi | 1 Gi |

chbackup uses memory primarily for:

- Buffering compressed parts before upload (up to part size, default < 256 MiB)
- S3 SDK internal buffers
- Decompression during download

If you use streaming multipart upload (parts > 256 MiB), memory usage stays bounded regardless of part size.

## Graceful shutdown

When Kubernetes sends SIGTERM to the pod, chbackup:

1. Stops accepting new operations
2. Waits for the current backup/upload/download/restore to finish its current part
3. Saves resume state (if enabled)
4. Shuts down cleanly

Set `terminationGracePeriodSeconds` long enough for the current operation to save state:

```yaml
spec:
  terminationGracePeriodSeconds: 60
```

For large backups where a single part upload might take minutes, increase this value. The operation can be resumed with `--resume` after the pod restarts.

## Multiple shards

For multi-shard ClickHouse clusters, use the `{shard}` macro in the S3 prefix to isolate each shard's backups:

```yaml
env:
  - name: S3_PREFIX
    value: "chbackup/{shard}"
```

Each chbackup sidecar reads the shard name from ClickHouse `system.macros` and writes to a separate prefix. The watch mode name template also supports `{shard}`:

```yaml
watch:
  name_template: "shard{shard}-{type}-{time:%Y%m%d_%H%M%S}"
```

This produces backup names like `shard01-full-20240115_120000`.

## Troubleshooting

### chbackup container keeps restarting

Check the logs:

```bash
kubectl logs -l app=clickhouse -c chbackup --previous
```

Common causes:

- **ClickHouse not ready**: chbackup tries to connect on startup. If ClickHouse is still initializing, it will fail. The readinessProbe will restart it. This is normal -- it should stabilize after ClickHouse is up.
- **Invalid S3 credentials**: Look for "Access Denied" in the logs. Verify the secret values.
- **Config file not found**: If using `-c` with a ConfigMap, check the mount path.

### "data_path does not exist"

The chbackup container cannot see `/var/lib/clickhouse`. Verify both containers mount the same volume:

```bash
kubectl describe pod clickhouse-xyz | grep -A5 "Mounts:"
```

### Watch mode not creating backups

Check watch status:

```bash
kubectl exec clickhouse-xyz -c chbackup -- chbackup list
curl http://localhost:7171/api/v1/watch/status  # after port-forward
```

If `consecutive_errors` is increasing, check the logs for the root cause (usually S3 or ClickHouse connectivity).

### Cannot reach the API from outside the pod

The default `api.listen` is `localhost:7171`, which only accepts connections from within the pod. For access from other pods or port-forward, set:

```yaml
env:
  - name: API_LISTEN
    value: "0.0.0.0:7171"
```

### Backup too slow

- Increase `CHBACKUP_UPLOAD_CONCURRENCY` (default: 4)
- Check if rate limiting is configured (`general.upload_max_bytes_per_second`)
- Verify the S3 bucket is in the same region as the cluster
- For many small tables, the bottleneck is per-request overhead -- consider `zstd` compression for better ratios

### Disk space issues

chbackup stores local backup data in `/var/lib/clickhouse/backup/`. If the volume fills up:

1. Set `watch.delete_local_after_upload: true` (default) to remove local copies after upload
2. Set `general.backups_to_keep_local: 1` to keep only one local backup
3. Use `general.backups_to_keep_local: -1` to delete immediately after upload

## Migrating from Go clickhouse-backup

chbackup is a drop-in replacement for `altinity/clickhouse-backup` in Kubernetes. Swap the sidecar image, keep your existing env vars, CronJobs, and URL engine tables — see the [Migration Guide](migration.md) for the full step-by-step walkthrough.
