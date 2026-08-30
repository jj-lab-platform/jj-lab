//! Default [`pkglab_common::Auth`] implementation: bcrypt-hashed Basic
//! credentials doubling as raw tokens, plus HMAC-SHA256 signed bearer tokens
//! (`base64url(payload).base64url(hmac)`), `X-NuGet-ApiKey` support, and
//! scope enforcement (`repository:name:action`).

use async_trait::async_trait;
use hmac::{Hmac, Mac};
#[allow(unused_imports)]
use pkglab_common::auth::Action as _Action;
use pkglab_common::auth::{scope, Auth, HeaderMap};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::digest::KeyInit;
use sha2::Sha256;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

#[derive(Serialize, Deserialize)]
struct TokenPayload {
    sub: String,
    scopes: Vec<String>,
    exp: i64,
}

pub struct DefaultAuth {
    users: HashMap<String, String>,
    tokens: HashMap<String, String>,
    signing_key: Vec<u8>,
    #[allow(dead_code)]
    ttl: u64,
}

impl DefaultAuth {
    /// Build from `user -> secret` pairs. Each secret is both the Basic
    /// password and a raw token that the same user can present directly.
    pub fn new(users: &[(String, String)]) -> Self {
        let mut hashed = HashMap::new();
        let mut tokens = HashMap::new();
        for (u, p) in users {
            let h = bcrypt::hash(p, bcrypt::DEFAULT_COST).unwrap_or_else(|_| p.clone());
            hashed.insert(u.clone(), h);
            tokens.insert(p.clone(), u.clone());
        }
        let mut key = vec![0u8; 32];
        rand::rng().fill_bytes(&mut key);
        Self { users: hashed, tokens, signing_key: key, ttl: 3600 }
    }

    pub fn with_signing_key(mut self, key: &[u8]) -> Self {
        if !key.is_empty() {
            self.signing_key = key.to_vec();
        }
        self
    }

    pub fn signing_key(&self) -> &[u8] {
        &self.signing_key
    }

    fn sign(&self, data: &[u8]) -> String {
        use base64::Engine as _;
        let mut mac = HmacSha256::new_from_slice(&self.signing_key)
            .expect("HMAC-SHA256 accepts any key length");
        mac.update(data);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok()
}

fn b64url_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn b64std_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn permits_any(granted: &[String], wanted: &str) -> bool {
    let Some((_, want_repo, want_act)) = split_scope(wanted) else {
        return false;
    };
    for g in granted {
        if let Some((_, repo, act)) = split_scope(g) {
            if repo != want_repo {
                continue;
            }
            if act == "*" || act == want_act {
                return true;
            }
            if act == "push" && want_act == "pull" {
                return true;
            }
        }
    }
    false
}

fn split_scope(s: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((parts[0].into(), parts[1].into(), parts[2].into()))
}

#[async_trait]
impl Auth for DefaultAuth {
    async fn authenticate(&self, headers: &HeaderMap) -> String {
        // Basic.
        if let Some(v) = headers.get(http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
            if let Some(rest) = v.strip_prefix("Basic ") {
                if let Some(decoded) = b64std_decode(rest.trim()) {
                    let s = String::from_utf8_lossy(&decoded);
                    if let Some((user, pass)) = s.split_once(':') {
                        if self.check_basic(user, pass).await {
                            return user.to_string();
                        }
                    }
                }
            }
            // Bearer JWT or raw token.
            if let Some(tok) = v.strip_prefix("Bearer ") {
                if let Some(u) = self.check_token(tok).await {
                    return u;
                }
                if let Some(u) =
                    self.check_bearer(tok, &scope("*", pkglab_common::auth::Action::All)).await
                {
                    return u;
                }
            }
        }
        // X-NuGet-ApiKey.
        if let Some(key) = headers.get("X-NuGet-ApiKey").and_then(|v| v.to_str().ok()) {
            if let Some(u) = self.check_token(key).await {
                return u;
            }
        }
        String::new()
    }

    async fn check_basic(&self, user: &str, pass: &str) -> bool {
        match self.users.get(user) {
            Some(hash) => bcrypt::verify(pass, hash).unwrap_or(false),
            None => false,
        }
    }

    async fn check_token(&self, token: &str) -> Option<String> {
        self.tokens.get(token).cloned()
    }

    async fn issue_token(&self, username: &str, scopes: &[String], ttl_seconds: u64) -> String {
        let payload = TokenPayload {
            sub: username.to_string(),
            scopes: scopes.to_vec(),
            exp: now_unix() + ttl_seconds as i64,
        };
        let body = serde_json::to_vec(&payload).unwrap_or_default();
        format!("{}.{}", b64url_encode(&body), self.sign(&body))
    }

    async fn check_bearer(&self, token: &str, wanted: &str) -> Option<String> {
        if let Some(u) = self.check_token(token).await {
            return Some(u);
        }
        let (raw, sig) = token.split_once('.')?;
        let raw_bytes = b64url_decode(raw)?;
        let sig_bytes = b64url_decode(sig)?;
        let mut mac = HmacSha256::new_from_slice(&self.signing_key).ok()?;
        mac.update(&raw_bytes);
        if mac.finalize().into_bytes().as_slice() != sig_bytes.as_slice() {
            return None;
        }
        let payload: TokenPayload = serde_json::from_slice(&raw_bytes).ok()?;
        if now_unix() > payload.exp {
            return None;
        }
        if !permits_any(&payload.scopes, wanted) {
            return None;
        }
        Some(payload.sub)
    }
}

/// Standard registry token endpoint handler shared by adapters: authenticates
/// the client (basic or anonymous for pull) and issues a scoped token.
pub async fn token_endpoint(
    auth: &std::sync::Arc<dyn Auth>,
    realm: &str,
    headers: &HeaderMap,
    query_scopes: &[String],
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let username = auth.authenticate(headers).await;
    for s in query_scopes {
        if let Some((_, _, act)) = split_scope(s) {
            if (act == "push" || act == "delete" || act == "*") && username.is_empty() {
                return (
                    StatusCode::UNAUTHORIZED,
                    [
                        ("WWW-Authenticate", format!("Basic realm=\"{realm}\"")),
                        ("Content-Type", "application/json".into()),
                    ],
                    serde_json::json!({
                        "errors": [{"code": "UNAUTHORIZED", "message": "authentication required"}]
                    })
                    .to_string(),
                )
                    .into_response();
            }
        }
    }
    let token = auth.issue_token(&username, query_scopes, 3600).await;
    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        serde_json::json!({
            "token": token,
            "access_token": token,
            "expires_in": 3600,
            "issued_at": chrono_like_now(),
        })
        .to_string(),
    )
        .into_response()
}

fn chrono_like_now() -> String {
    // RFC3339 UTC seconds precision without pulling chrono: good enough for
    // informational `issued_at`.
    let secs = now_unix();
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}
