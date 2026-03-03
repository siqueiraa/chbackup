//! Canonical path component encoding, sanitization, and disk path validation.
//!
//! Provides a single, consistent percent-encoding function for path components
//! (database names, table names) used throughout the codebase. Replaces the
//! four duplicated `url_encode` implementations that existed in backup/collect.rs,
//! download/mod.rs, upload/mod.rs, and restore/attach.rs.
//!
//! Also provides [`validate_disk_path`] for two-tier validation of disk paths
//! from manifest data before using them for local filesystem operations.
//!
//! Key design decisions:
//! - Does NOT preserve `/` (all callers pass individual db or table names)
//! - Uses byte-level encoding for multi-byte UTF-8 characters
//! - `sanitize_path_component` explicitly rejects `""`, `"."`, `".."` to prevent
//!   path traversal attacks
//! - `validate_disk_path` rejects paths pointing to system directories, even
//!   through symlinks

use std::path::Path;

/// Percent-encode a path component (database name, table name, etc.).
///
/// Safe characters that are preserved: alphanumeric, `-`, `_`, `.`
/// All other characters (including `/`) are percent-encoded using byte-level
/// encoding for multi-byte UTF-8 characters.
///
/// # Examples
///
/// ```
/// use chbackup::path_encoding::encode_path_component;
///
/// assert_eq!(encode_path_component("default"), "default");
/// assert_eq!(encode_path_component("my table"), "my%20table");
/// assert_eq!(encode_path_component("db/table"), "db%2Ftable");
/// ```
pub fn encode_path_component(s: &str) -> String {
    let mut encoded = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
            encoded.push(c);
        } else {
            // Byte-level encoding for all non-safe characters (including multi-byte UTF-8)
            for byte in c.to_string().as_bytes() {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

/// Sanitize a path component before encoding.
///
/// Returns `""` for empty strings, `"."`, and `".."` (explicit rejection to
/// prevent path traversal). Strips leading `/` characters. Then delegates
/// to [`encode_path_component`] for percent-encoding.
///
/// Callers that split on `/` and iterate components must skip empty returns.
///
/// # Examples
///
/// ```
/// use chbackup::path_encoding::sanitize_path_component;
///
/// assert_eq!(sanitize_path_component("default"), "default");
/// assert_eq!(sanitize_path_component(".."), "");
/// assert_eq!(sanitize_path_component("/foo"), "foo");
/// ```
pub fn sanitize_path_component(s: &str) -> String {
    // Explicitly reject dangerous path components
    if s.is_empty() || s == "." || s == ".." {
        return String::new();
    }

    // Strip leading slashes
    let stripped = s.trim_start_matches('/');

    encode_path_component(stripped)
}

/// Known system directories that must never be used as disk paths.
///
/// Any path that equals or is a subdirectory of these is rejected by
/// [`validate_disk_path`].
const FORBIDDEN_PREFIXES: &[&str] = &[
    "/etc", "/root", "/sys", "/proc", "/dev", "/boot", "/bin", "/sbin", "/usr", "/lib", "/lib64",
    "/run",
];

/// Check whether a path equals or is under any known system directory.
///
/// Returns `true` if the path is forbidden (i.e., it equals or starts_with
/// one of the [`FORBIDDEN_PREFIXES`]). Also checks against the canonical
/// (symlink-resolved) form of each prefix, which handles platforms like macOS
/// where `/etc` is a symlink to `/private/etc`.
fn check_forbidden(path: &Path) -> bool {
    for prefix in FORBIDDEN_PREFIXES {
        let prefix_path = Path::new(prefix);
        if path == prefix_path || path.starts_with(prefix_path) {
            return true;
        }
        // Also check against the canonical form of the forbidden prefix
        // (e.g., /etc -> /private/etc on macOS)
        if let Ok(canonical_prefix) = std::fs::canonicalize(prefix_path) {
            if path == canonical_prefix || path.starts_with(&canonical_prefix) {
                return true;
            }
        }
    }
    false
}

/// Validate a disk path from a backup manifest before using it for local
/// filesystem operations (download target directory, delete).
///
/// Two-tier validation:
///
/// **Tier 1 -- String checks** (fast, no I/O):
/// - Must be an absolute path (starts with `/`)
/// - Must not contain `..` path components
/// - Must not be filesystem root `/`
/// - Must not equal or be under a known system directory
///
/// **Tier 2 -- Canonical path checks** (when path exists on disk):
/// - `std::fs::canonicalize()` resolves symlinks; if the resolved path is
///   under a system directory, the path is rejected. This catches symlink
///   attacks like `/data/evil -> /etc`.
/// - If `canonicalize` fails (path does not exist), tier 1 is sufficient.
///
/// Returns `true` if the path is safe to use, `false` if it should be
/// rejected with a fallback to the default backup directory.
pub fn validate_disk_path(path: &str) -> bool {
    // Tier 1: String-level checks (no I/O)

    // Must be absolute
    if !path.starts_with('/') {
        return false;
    }

    // Normalize: strip trailing slashes for consistent comparison
    let normalized = path.trim_end_matches('/');

    // Must not be filesystem root
    if normalized.is_empty() {
        return false;
    }

    // Must not contain ".." path components
    for component in normalized.split('/') {
        if component == ".." {
            return false;
        }
    }

    let p = Path::new(normalized);

    // Must not be under a known system directory
    if check_forbidden(p) {
        return false;
    }

    // Tier 2: Canonical path checks (resolve symlinks if path exists)
    if let Ok(canonical) = std::fs::canonicalize(p) {
        if check_forbidden(&canonical) {
            return false;
        }
    }
    // If canonicalize fails (path doesn't exist), tier 1 was sufficient

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_path_component_basic() {
        // Alphanumeric preserved
        assert_eq!(encode_path_component("default"), "default");
        assert_eq!(encode_path_component("my_table"), "my_table");
        assert_eq!(encode_path_component("backup-2024"), "backup-2024");
        assert_eq!(encode_path_component("v1.0"), "v1.0");
        assert_eq!(encode_path_component("ABC123"), "ABC123");

        // Spaces and special chars encoded
        assert_eq!(encode_path_component("my table"), "my%20table");
        assert_eq!(encode_path_component("a+b"), "a%2Bb");
        assert_eq!(encode_path_component("a@b"), "a%40b");
        assert_eq!(encode_path_component("a#b"), "a%23b");
        assert_eq!(encode_path_component("a?b"), "a%3Fb");
        assert_eq!(encode_path_component("a=b"), "a%3Db");
    }

    #[test]
    fn test_encode_path_component_no_slash_preservation() {
        // `/` must be encoded (key difference from old url_encode_path)
        assert_eq!(encode_path_component("db/table"), "db%2Ftable");
        assert_eq!(encode_path_component("/leading"), "%2Fleading");
        assert_eq!(encode_path_component("a/b/c"), "a%2Fb%2Fc");
    }

    #[test]
    fn test_encode_path_component_multibyte_utf8() {
        // Multi-byte UTF-8 chars that are NOT alphanumeric use byte-level encoding.
        // Note: Rust's is_alphanumeric() follows Unicode -- CJK chars ARE alphanumeric
        // and are preserved (same as existing url_encode_path in collect.rs).

        // Combining acute (U+0301) is NOT alphanumeric -> byte-encoded
        let input = "cafe\u{0301}";
        let encoded = encode_path_component(input);
        assert!(
            encoded.contains("%CC%81"),
            "Multi-byte combining accent should be byte-encoded, got: {}",
            encoded
        );

        // Japanese kanji (U+65E5) IS alphanumeric in Unicode -> preserved
        let encoded_jp = encode_path_component("\u{65E5}");
        assert_eq!(encoded_jp, "\u{65E5}");

        // Emoji (U+1F600) is NOT alphanumeric -> byte-encoded
        let encoded_emoji = encode_path_component("\u{1F600}");
        assert_eq!(encoded_emoji, "%F0%9F%98%80");

        // Non-alphanumeric multi-byte: em dash (U+2014) -> byte-encoded
        let encoded_dash = encode_path_component("\u{2014}");
        assert_eq!(encoded_dash, "%E2%80%94");
    }

    #[test]
    fn test_sanitize_path_component_blocks_dotdot() {
        assert_eq!(sanitize_path_component(".."), "");
    }

    #[test]
    fn test_sanitize_path_component_blocks_dot() {
        assert_eq!(sanitize_path_component("."), "");
    }

    #[test]
    fn test_sanitize_path_component_strips_leading_slash() {
        assert_eq!(sanitize_path_component("/foo"), "foo");
        assert_eq!(sanitize_path_component("//bar"), "bar");
        assert_eq!(sanitize_path_component("///baz"), "baz");
    }

    #[test]
    fn test_sanitize_path_component_normal_names() {
        // Normal db/table names pass through encoding unchanged
        assert_eq!(sanitize_path_component("default"), "default");
        assert_eq!(sanitize_path_component("my_database"), "my_database");
        assert_eq!(sanitize_path_component("trades-2024"), "trades-2024");
        assert_eq!(sanitize_path_component("system.parts"), "system.parts");
        // Empty string returns empty
        assert_eq!(sanitize_path_component(""), "");
    }

    #[test]
    fn test_encode_path_component_empty() {
        assert_eq!(encode_path_component(""), "");
    }

    #[test]
    fn test_sanitize_path_component_encoded_dotdot() {
        // "..." (three dots) is NOT ".." so it should be encoded normally
        // But "." chars are safe, so "..." stays as "..."
        assert_eq!(sanitize_path_component("..."), "...");
    }

    // --- validate_disk_path tests ---

    #[test]
    fn test_validate_valid_paths() {
        assert!(validate_disk_path("/var/lib/clickhouse"));
        assert!(validate_disk_path("/mnt/nvme1/clickhouse"));
        assert!(validate_disk_path("/data/disks/fast"));
        assert!(validate_disk_path("/opt/clickhouse/data"));
        assert!(validate_disk_path("/home/clickhouse/storage"));
    }

    #[test]
    fn test_validate_reject_relative() {
        assert!(!validate_disk_path("relative/path"));
        assert!(!validate_disk_path("./data"));
        assert!(!validate_disk_path("data"));
    }

    #[test]
    fn test_validate_reject_dotdot() {
        assert!(!validate_disk_path("/var/lib/../../etc"));
        assert!(!validate_disk_path("/data/../etc/passwd"));
        assert!(!validate_disk_path("/.."));
    }

    #[test]
    fn test_validate_reject_root() {
        assert!(!validate_disk_path("/"));
        assert!(!validate_disk_path("///"));
    }

    #[test]
    fn test_validate_reject_system_dirs() {
        assert!(!validate_disk_path("/etc"));
        assert!(!validate_disk_path("/etc/clickhouse-server"));
        assert!(!validate_disk_path("/root"));
        assert!(!validate_disk_path("/sys"));
        assert!(!validate_disk_path("/proc"));
        assert!(!validate_disk_path("/dev"));
        assert!(!validate_disk_path("/boot"));
        assert!(!validate_disk_path("/bin"));
        assert!(!validate_disk_path("/sbin"));
        assert!(!validate_disk_path("/usr"));
        assert!(!validate_disk_path("/usr/local/bin"));
        assert!(!validate_disk_path("/lib"));
        assert!(!validate_disk_path("/lib64"));
        assert!(!validate_disk_path("/run"));
    }

    #[test]
    fn test_validate_allow_similar_names() {
        // Paths that START with characters similar to forbidden prefixes but
        // are NOT actually under them (different directory names).
        assert!(validate_disk_path("/etcdata/clickhouse"));
        assert!(validate_disk_path("/users/clickhouse"));
        assert!(validate_disk_path("/library/data"));
        assert!(validate_disk_path("/running/data"));
        assert!(validate_disk_path("/bootstrap/data"));
        assert!(validate_disk_path("/devices/storage"));
    }

    #[test]
    fn test_validate_reject_symlink_to_system() {
        // Use tempfile for proper isolated temp directory (no collision under parallel tests)
        let tmp = tempfile::tempdir().unwrap();
        let symlink_path = tmp.path().join("evil_link");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", &symlink_path).unwrap();
            assert!(
                !validate_disk_path(symlink_path.to_str().unwrap()),
                "Symlink to /etc should be rejected"
            );
        }
        // tmp dropped here -> auto-cleanup
    }

    #[test]
    fn test_validate_trailing_slash() {
        // Trailing slashes should be stripped before validation
        assert!(validate_disk_path("/var/lib/clickhouse/"));
        assert!(!validate_disk_path("/etc/"));
        assert!(!validate_disk_path("/usr/"));
    }

    #[test]
    fn test_check_forbidden_helper() {
        assert!(check_forbidden(Path::new("/etc")));
        assert!(check_forbidden(Path::new("/etc/clickhouse")));
        assert!(check_forbidden(Path::new("/usr")));
        assert!(check_forbidden(Path::new("/usr/local/bin")));
        assert!(!check_forbidden(Path::new("/var/lib/clickhouse")));
        assert!(!check_forbidden(Path::new("/data")));
        assert!(!check_forbidden(Path::new("/mnt/nvme")));
    }
}
