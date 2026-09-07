//! Bearer-token gate for the MCP endpoint. Same idea as the llama-server
//! sidecar's per-spawn API key: the listener is loopback-only, and the token
//! additionally rejects rogue local processes and DNS-rebinding browser
//! requests (a cross-origin `fetch` cannot attach `Authorization` without a
//! CORS preflight we never answer).

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;

/// axum middleware: pass through only when `Authorization: Bearer <token>`
/// matches `expected`; otherwise 401 with a `WWW-Authenticate` challenge.
pub async fn require_bearer(
    State(expected): State<Arc<String>>,
    req: Request,
    next: Next,
) -> Response {
    let header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if bearer_matches(header, &expected) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer realm=\"lilnotes-mcp\"")],
            "missing or invalid bearer token",
        )
            .into_response()
    }
}

/// `Bearer <token>` (scheme case-insensitive, surrounding whitespace
/// ignored) compared in constant time. A length mismatch is a plain
/// `false` — the length of the expected token is not secret.
pub fn bearer_matches(header: Option<&str>, expected: &str) -> bool {
    let Some(h) = header else {
        return false;
    };
    let h = h.trim();
    let Some((scheme, token)) = h.split_once(char::is_whitespace) else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return false;
    }
    let token = token.trim();
    if token.is_empty() || expected.is_empty() {
        return false;
    }
    token.as_bytes().ct_eq(expected.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::bearer_matches;

    const T: &str = "0123456789abcdef";

    #[test]
    fn accepts_exact_token() {
        assert!(bearer_matches(Some("Bearer 0123456789abcdef"), T));
        assert!(bearer_matches(Some("bearer 0123456789abcdef"), T));
        assert!(bearer_matches(Some("  Bearer   0123456789abcdef  "), T));
    }

    #[test]
    fn rejects_everything_else() {
        assert!(!bearer_matches(None, T));
        assert!(!bearer_matches(Some(""), T));
        assert!(!bearer_matches(Some("Bearer"), T));
        assert!(!bearer_matches(Some("Bearer "), T));
        assert!(!bearer_matches(Some("Basic 0123456789abcdef"), T));
        assert!(!bearer_matches(Some("Bearer 0123456789abcde"), T));
        assert!(!bearer_matches(Some("Bearer 0123456789abcdeg"), T));
        assert!(!bearer_matches(Some("Bearer 0123456789abcdef0"), T));
        assert!(!bearer_matches(Some("Bearer x"), ""));
    }
}
