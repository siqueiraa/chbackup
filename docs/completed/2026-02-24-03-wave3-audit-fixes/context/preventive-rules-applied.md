# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md` (35 rules)
- `.claude/skills/self-healing/references/planning-rules.md` (14 rules)

## Rules Applied to This Plan

| Rule | Relevance | Application |
|------|-----------|-------------|
| RC-001 | N/A | No actor dependencies in this plan |
| RC-002 | LOW | No type comments to trust -- verified via LSP hover |
| RC-004 | MEDIUM | W3-3 adds WatchStartRequest -- needs handler AND sender (API client) |
| RC-006 | HIGH | Verified all API/method signatures via LSP hover before documenting |
| RC-007 | N/A | No tuple field order assumptions |
| RC-008 | MEDIUM | W3-5 adds CLI fields that must exist before main.rs wiring can reference them |
| RC-021 | HIGH | Verified all file locations via direct file reads |
| RC-035 | MEDIUM | cargo fmt must be run after changes |

## Verification Actions Taken

1. **RC-006**: Used LSP hover on `parse_duration_secs` (confirmed `pub fn parse_duration_secs(s: &str) -> Result<u64>`), `Config::validate` (confirmed `pub fn validate(&self) -> Result<()>`), `watch_start` (confirmed current signature), `spawn_watch_from_state` (confirmed `pub async fn spawn_watch_from_state(state: &mut AppState, config_path: PathBuf, macros: HashMap<String, String>)`), `start_server` (confirmed full signature)
2. **RC-021**: Read all 5 target files at the exact lines mentioned in the audit findings
3. **RC-008**: W3-5 CLI changes must be compiled before main.rs wiring -- task sequencing required
4. **RC-004**: W3-3 `WatchStartRequest` struct needs `#[derive(Debug, Deserialize, Default)]` to match existing request type patterns in routes.rs
