# Affected Modules Analysis

## Summary

- **Module directories modified:** 3 (src/restore, src/watch, src/server)
- **Standalone files modified:** 3 (src/config.rs, src/cli.rs, src/main.rs)
- **CLAUDE.md updates needed:** 2 (src/watch, src/server)
- **CLAUDE.md creates needed:** 0

## Files Being Modified

| File | Module | CLAUDE.md | Finding | Change Description |
|------|--------|-----------|---------|-------------------|
| src/restore/remap.rs | src/restore | EXISTS | W3-1 | Fix `&&` to `\|\|` at line 647, add regression test |
| src/watch/mod.rs | src/watch | EXISTS | W3-2 | Add `classify_backup_type()` helper, replace `.contains("full")` heuristic |
| src/server/routes.rs | src/server | EXISTS | W3-3 | Add `WatchStartRequest` type, modify `watch_start` to accept optional body |
| src/config.rs | (root) | N/A | W3-4 | Remove `if self.watch.enabled` gate from interval validation |
| src/cli.rs | (root) | N/A | W3-5 | Add `watch_interval`/`full_interval` to `Command::Server` |
| src/main.rs | (root) | N/A | W3-5 | Wire Server interval flags into config before `start_server()` |

## CLAUDE.md Update Analysis

### src/watch/CLAUDE.md -- UPDATE needed
- **Trigger:** New `classify_backup_type()` public function
- **Update scope:** Add to Public API section, document the helper

### src/server/CLAUDE.md -- UPDATE needed
- **Trigger:** New `WatchStartRequest` type, modified `watch_start` handler signature
- **Update scope:** Update Watch API endpoints section to document optional body parameter

### src/restore/CLAUDE.md -- NO UPDATE needed
- The fix is a single operator change in a private function; no new patterns or API changes
