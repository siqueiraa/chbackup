# Preventive Rules Applied

## Rules Loaded From
- `.claude/skills/self-healing/references/root-causes.md`
- `.claude/skills/self-healing/references/planning-rules.md`

## Applicable Rules and How They Apply

| Rule | Applicability | Action |
|------|--------------|--------|
| RC-006 | HIGH - Plan code snippets must use verified APIs | Verify all API/method calls in code sketches against actual codebase |
| RC-008 | HIGH - TDD sequencing | Ensure fields exist or are added in preceding task before test code references them |
| RC-018 | MEDIUM - TDD task test steps | Every task modifying behavior must have explicit test steps |
| RC-019 | HIGH - Follow existing patterns | New code (sanitizer, env-key translator) must follow existing patterns |
| RC-021 | HIGH - Verify struct/field locations | All file locations verified with grep, not assumed |
| RC-032 | LOW - Data authority | Not applicable (no tracking/calculation being added) |
| RC-035 | MEDIUM - cargo fmt | Run before committing |

## Rules NOT Applicable (with reason)

| Rule | Reason |
|------|--------|
| RC-001 | No actor dependencies |
| RC-002 | No financial data types |
| RC-004 | No message handlers |
| RC-010 | No adapter methods |
| RC-011 | No state flags |
| RC-020 | No Kameo message types |

## Key Verification Actions Taken

1. **RC-006**: All url_encode function signatures verified via grep
2. **RC-021**: All struct/file locations verified (ColumnInconsistency at client.rs:71, S3Config at config.rs:565, S3Client at s3.rs:43)
3. **RC-019**: Existing `apply_env_overlay()` pattern examined for env-key translation (issue 6)
4. **RC-008**: Task ordering ensures shared `sanitize_path` module (issue 7/DRY) is created before consumers (issue 1/path traversal)
