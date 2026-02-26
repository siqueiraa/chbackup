//! Basic authentication middleware for the API server.
//!
//! When `api.username` and `api.password` are both non-empty in the config,
//! all endpoints require HTTP Basic authentication. If both are empty,
//! requests pass through without authentication.

use axum::{
    body::Body,
    extract::State,
    http::{header, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};

use super::state::AppState;

/// Axum middleware that enforces HTTP Basic authentication.
///
/// If `config.api.username` and `config.api.password` are both empty,
/// requests pass through without authentication. Otherwise, the request
/// must include a valid `Authorization: Basic <base64>` header.
pub async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let config = state.config.load();
    let username = &config.api.username;
    let password = &config.api.password;

    // No auth required if both username and password are empty
    if username.is_empty() && password.is_empty() {
        return next.run(request).await;
    }

    // Extract Authorization header
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let Some(auth_value) = auth_header else {
        return unauthorized_response();
    };

    // Must start with "Basic "
    let Some(encoded) = auth_value.strip_prefix("Basic ") else {
        return unauthorized_response();
    };

    // Decode base64
    let Ok(decoded_bytes) = STANDARD.decode(encoded) else {
        return unauthorized_response();
    };

    let Ok(decoded) = String::from_utf8(decoded_bytes) else {
        return unauthorized_response();
    };

    // Split on first ':' to get username:password
    let Some((req_user, req_pass)) = decoded.split_once(':') else {
        return unauthorized_response();
    };

    // Compare credentials using constant-time comparison to prevent timing attacks.
    // Evaluate both comparisons to avoid short-circuit leaking which credential failed.
    let user_ok = constant_time_eq(req_user.as_bytes(), username.as_bytes());
    let pass_ok = constant_time_eq(req_pass.as_bytes(), password.as_bytes());
    if user_ok & pass_ok {
        next.run(request).await
    } else {
        unauthorized_response()
    }
}

/// Constant-time byte comparison to prevent timing attacks.
///
/// Returns `true` only when both slices have the same length AND identical
/// contents. For HTTP Basic auth credentials, leaking length via the early
/// return is acceptable (credential length is bounded and chosen by the
/// operator). The XOR-fold loop remains constant-time with respect to
/// content, which is the security-relevant property.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Build a 401 Unauthorized response with WWW-Authenticate header.
fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"chbackup\"")],
        "Unauthorized",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_decode_valid_credentials() {
        let encoded = STANDARD.encode("admin:secret123");
        let decoded_bytes = STANDARD.decode(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();
        match decoded.split_once(':') {
            Some((user, pass)) => {
                assert_eq!(user, "admin");
                assert_eq!(pass, "secret123");
            }
            None => panic!("expected colon-separated credentials, got: {decoded}"),
        }
    }

    #[test]
    fn test_auth_decode_empty_credentials() {
        let encoded = STANDARD.encode(":");
        let decoded_bytes = STANDARD.decode(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();
        match decoded.split_once(':') {
            Some((user, pass)) => {
                assert_eq!(user, "");
                assert_eq!(pass, "");
            }
            None => panic!("expected colon-separated credentials, got: {decoded}"),
        }
    }

    #[test]
    fn test_auth_decode_password_with_colon() {
        // Password may contain colons; split_once ensures only first colon is used
        let encoded = STANDARD.encode("user:pass:with:colons");
        let decoded_bytes = STANDARD.decode(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();
        match decoded.split_once(':') {
            Some((user, pass)) => {
                assert_eq!(user, "user");
                assert_eq!(pass, "pass:with:colons");
            }
            None => panic!("expected colon-separated credentials, got: {decoded}"),
        }
    }

    #[test]
    fn test_auth_decode_no_colon_returns_none() {
        // Malformed input without a colon should not be accepted
        let encoded = STANDARD.encode("nocolonhere");
        let decoded_bytes = STANDARD.decode(&encoded).unwrap();
        let decoded = String::from_utf8(decoded_bytes).unwrap();
        assert!(
            decoded.split_once(':').is_none(),
            "expected None for input without colon"
        );
    }

    #[test]
    fn test_auth_no_config() {
        // When both username and password are empty, auth should be skipped
        let config = crate::config::Config::default();
        assert!(config.api.username.is_empty());
        assert!(config.api.password.is_empty());
        // The middleware will call next.run() when both are empty
    }

    #[test]
    fn test_auth_invalid_base64() {
        let result = STANDARD.decode("not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_unauthorized_response_status() {
        let response = unauthorized_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_constant_time_eq_equal() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"\x00\x01\x02", b"\x00\x01\x02"));
    }

    #[test]
    fn test_constant_time_eq_different_content() {
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"abc", b"abd"));
    }

    #[test]
    fn test_constant_time_eq_different_length() {
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"hi", b"hello"));
        assert!(!constant_time_eq(b"", b"a"));
    }
}
