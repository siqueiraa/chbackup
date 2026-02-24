//! Canonical path component encoding and sanitization.
//!
//! Provides a single, consistent percent-encoding function for path components
//! (database names, table names) used throughout the codebase. Replaces the
//! four duplicated `url_encode` implementations that existed in backup/collect.rs,
//! download/mod.rs, upload/mod.rs, and restore/attach.rs.
//!
//! Key design decisions:
//! - Does NOT preserve `/` (all callers pass individual db or table names)
//! - Uses byte-level encoding for multi-byte UTF-8 characters
//! - `sanitize_path_component` explicitly rejects `""`, `"."`, `".."` to prevent
//!   path traversal attacks

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
}
