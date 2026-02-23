# Redundancy Analysis

## Proposed New Public Components

### 1. `sanitize_path_component(s: &str) -> String` (or similar)

**Search**: `url_encode_path`, `url_encode`, `url_encode_component`, `sanitize_name`

| Proposed | Existing Match (fully qualified) | Decision | Task/Acceptance | Justification |
|----------|----------------------------------|----------|-----------------|---------------|
| `path_encoding::encode_path_component()` | `backup::collect::url_encode_path()` | REPLACE | Issue 7 task / all consumers updated | New function adds `..` blocking, strips leading slashes, and is the SINGLE canonical encoder |
| `path_encoding::encode_path_component()` | `download::url_encode()` | REPLACE | Issue 7 task / private fn removed | Download's private copy is redundant |
| `path_encoding::encode_path_component()` | `upload::url_encode_component()` | REPLACE | Issue 7 task / private fn removed | Upload's private copy is redundant; note upload does NOT preserve `/` which is correct behavior |
| `path_encoding::encode_path_component()` | `restore::attach::url_encode()` | REPLACE | Issue 7 task / pub(crate) fn removed | Restore's copy is redundant |
| `path_encoding::sanitize_path_component()` | `clickhouse::client::sanitize_name()` | COEXIST | N/A | `sanitize_name` is for ClickHouse identifiers (freeze names), not for path encoding. Different purpose. |

**REPLACE details**:
- Removal of old code: same task that introduces the new module
- Test migration: existing tests of `url_encode_path` will be migrated to new module
- Acceptance criteria: `grep -rn 'fn url_encode' src/` returns only the new canonical function

### 2. `env_key_to_dot_notation(key: &str) -> Option<&str>` (or similar)

**Search**: `set_field`, `apply_env_overlay`, `apply_cli_env_overrides`

| Proposed | Existing Match | Decision | Justification |
|----------|----------------|----------|---------------|
| `env_key_to_dot_notation()` | `apply_env_overlay()` hardcoded mapping | EXTEND | Extracts the mapping that already exists in `apply_env_overlay()` into a reusable lookup |

No REPLACE needed -- `apply_env_overlay()` continues to work as-is for process env vars. The new function is an extension used only by `apply_cli_env_overrides()`.

### 3. No new structs or enums proposed

All issues modify behavior of existing types. No new public structs, enums, or traits.
