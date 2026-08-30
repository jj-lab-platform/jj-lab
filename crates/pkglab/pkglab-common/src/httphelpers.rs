//! Shared HTTP response/helper builders used by every protocol adapter.
//!
//! Each adapter previously declared its own `json`/`text`/`error`/
//! `blob_response`/`authorize_write`/`urlencode`, all byte-for-byte identical
//! (or trivially different). They are consolidated here so the adapters stay
//! small and the exact wire format is defined in one place.

use crate::auth::HeaderMap as AuthHeaders;
use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

/// `application/json` response from a `serde_json::Value`.
pub fn json(status: StatusCode, v: serde_json::Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json".to_string())], v.to_string())
        .into_response()
}

/// Plain-`text/*` response with an explicit content type.
pub fn text(status: StatusCode, body: String, ct: &str) -> Response {
    (status, [(header::CONTENT_TYPE, ct.to_string())], body).into_response()
}

/// `{"ok": false, "error": msg}` error response.
pub fn error(status: StatusCode, msg: &str) -> Response {
    json(status, serde_json::json!({"ok": false, "error": msg}))
}

/// `application/octet-stream` blob with a content-disposition filename.
pub fn blob_response(data: Vec<u8>, filename: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, data.len())
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))
        .body(Body::from(data))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `application/octet-stream` blob without a filename disposition.
pub fn octet_response(data: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, data.len())
        .body(Body::from(data))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Gate write operations on an optional [`crate::Auth`]. Returns the 401
/// challenge response to surface to the client, or `Ok(())`.
pub async fn authorize_write(
    auth: &Option<std::sync::Arc<dyn crate::Auth>>,
    headers: &HeaderMap,
) -> Result<(), Response> {
    if let Some(a) = auth {
        let converted: AuthHeaders = headers.clone();
        if a.authorize_write(&converted).await.is_none() {
            return Err((
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Basic realm=\"registry\"".to_string())],
                serde_json::json!({"ok": false, "error": "authentication required"}).to_string(),
            )
                .into_response());
        }
    }
    Ok(())
}

/// Percent-encode a path segment: the RFC 3986 unreserved set (plus the
/// common `~`) stays literal, everything else is `%XX` uppercase.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
