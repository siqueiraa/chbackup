# Data Authority Analysis

## Summary

All 5 findings are code correctness fixes or small feature additions. No new data tracking or computation is required -- all data sources already exist.

| Data Needed | Source Type | Field Available | Decision |
|-------------|-------------|-----------------|----------|
| Backup type classification (full/incr) | Name template `{type}` substitution | resolve_name_template substitutes "full" or "incr" into template | USE EXISTING -- derive type by reverse-matching template pattern against backup name |
| Watch interval / full interval | WatchConfig fields | watch_interval: String, full_interval: String | USE EXISTING -- already in Config struct |
| Duration parsing | parse_duration_secs() | Returns u64 seconds | USE EXISTING |
| Distributed engine args (db, table) | DDL string parsing | Extracted via splitn + strip_quotes in rewrite_distributed_engine | USE EXISTING -- fix logic operator only |
| ClickHouse macros | ChClient::get_macros() | Returns HashMap<String,String> | USE EXISTING -- already called in watch_start |

## Analysis Notes

- W3-1 is a pure logic bug fix -- no new data sources needed
- W3-2 needs to classify backup names by type, which can be derived from the existing name template by replacing `{type}` with "full"/"incr" and checking if the backup name matches the pattern. The template and `resolve_name_template` function already exist.
- W3-3 needs to accept optional JSON body in watch_start -- the data comes from the HTTP request body (new input, not a data source)
- W3-4 removes a conditional gate -- no new data needed
- W3-5 adds CLI flags that map to existing WatchConfig fields
