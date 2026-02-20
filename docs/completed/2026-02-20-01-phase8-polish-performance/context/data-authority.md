# Data Authority Analysis

## Gap 1: rbac_size / config_size

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| RBAC backup size | Local filesystem | `{backup_dir}/access/` directory | MUST IMPLEMENT -- walkdir scan at backup time |
| Config backup size | Local filesystem | `{backup_dir}/configs/` directory | MUST IMPLEMENT -- walkdir scan at backup time |
| rbac_size in manifest | BackupManifest | Not present | MUST IMPLEMENT -- add field to BackupManifest |
| config_size in manifest | BackupManifest | Not present | MUST IMPLEMENT -- add field to BackupManifest |
| rbac_size in list | BackupSummary | Not present | MUST IMPLEMENT -- add field, read from manifest |
| rbac_size in API | ListResponse | `rbac_size: u64` (hardcoded 0) | USE EXISTING field -- just wire data through |
| config_size in API | ListResponse | `config_size: u64` (hardcoded 0) | USE EXISTING field -- just wire data through |

**Justification for MUST IMPLEMENT**: The filesystem directories `access/` and `configs/` exist only at backup creation time on the local host. There is no API that provides these sizes. They must be computed via walkdir during `backup::create()` and stored in the manifest for later retrieval by `list` and API endpoints.

## Gap 2: API tables pagination

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Table list | ChClient.list_tables() | Vec<TableRow> | USE EXISTING |
| Table filter | TablesParams.table | glob pattern | USE EXISTING |
| Page offset | TablesParams | Not present | MUST IMPLEMENT -- add `offset` query param |
| Page limit | TablesParams | Not present | MUST IMPLEMENT -- add `limit` query param |

**Justification**: Pagination is a server-side concern. ClickHouse returns all tables; we slice after filtering.

## Gap 3: Remote manifest caching

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Remote backup list | S3Client.list_common_prefixes() | Vec<String> | USE EXISTING |
| Manifest JSON | S3Client.get_object() | Vec<u8> per backup | USE EXISTING |
| Cached manifests | N/A | Not present | MUST IMPLEMENT -- in-memory HashMap<String, BackupManifest> with TTL |
| Cache invalidation | Mutating operations | N/A | MUST IMPLEMENT -- clear after upload/delete/retention |

**Justification**: Design doc 8.2 explicitly specifies caching: "cache manifest key-sets in memory when running in watch/server mode. On each cycle, only fetch manifests created since last cache refresh." No existing caching mechanism exists.

## Gap 4: SIGQUIT stack dump

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| SIGQUIT signal | tokio::signal::unix | SignalKind::quit() | USE EXISTING -- tokio provides this |
| Task stack dump | std::backtrace | Backtrace::capture() | USE EXISTING -- std library provides this |

**Justification**: Both components exist in the standard library and tokio. Only wiring is needed.

## Gap 5: Streaming multipart upload

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| S3 multipart API | S3Client | create_multipart_upload, upload_part, complete_multipart_upload | USE EXISTING |
| Compressed data | compress_part() | Returns Vec<u8> | MUST MODIFY -- need streaming variant |
| Part size threshold | Config | Not present (hardcoded MULTIPART_THRESHOLD) | USE EXISTING -- can reuse or add config field |

**Justification**: The existing buffered approach works but requires holding the entire compressed part in memory. For parts >256MB uncompressed (estimated 64-128MB compressed), streaming avoids memory spikes. The S3 multipart API already exists; only the compression pipeline needs a streaming variant.

## Analysis Notes

- Gap 1 is purely about data plumbing -- compute sizes at backup time, store in manifest, pass through to list/API
- Gap 2 is a simple query param extension following existing patterns
- Gap 3 requires a new shared cache struct in server state, with careful invalidation
- Gap 4 is minimal -- one signal handler spawn per signal context (server + standalone watch)
- Gap 5 is the most complex -- requires a new streaming compression pipeline alongside the existing buffered one
