use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

/// Shared auth config loaded once at boot.
#[derive(Clone, Default)]
pub struct AuthConfig {
    /// If `Some`, all protected routes require this key.
    pub api_key: Option<String>,
    /// Allowed browser origins for CORS (empty = permissive, for local only).
    pub cors_origins: Vec<String>,
    /// Serve docs HTML at `/`. Default true. Set ACEND_PUBLIC_UI=false to 404 `/`.
    pub public_ui: bool,
}

impl AuthConfig {
    pub fn from_env() -> Self {
        let api_key = std::env::var("ACEND_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let cors_origins = std::env::var("ACEND_CORS_ORIGINS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|o| o.trim().to_string())
                    .filter(|o| !o.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Docs at `/` are on by default (even with API key). Opt out with ACEND_PUBLIC_UI=false.
        let public_ui = match std::env::var("ACEND_PUBLIC_UI") {
            Ok(v) => !matches!(v.to_lowercase().as_str(), "0" | "false" | "no"),
            Err(_) => true,
        };

        Self {
            api_key,
            cors_origins,
            public_ui,
        }
    }

    pub fn check_key(&self, provided: Option<&str>) -> bool {
        match &self.api_key {
            None => true,
            Some(expected) => provided.is_some_and(|got| got == expected),
        }
    }
}

pub fn extract_api_key(headers: &HeaderMap, query_key: Option<&str>) -> Option<String> {
    if let Some(k) = query_key {
        let t = k.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(v) = headers.get("x-acend-key").and_then(|v| v.to_str().ok()) {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Some(v) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if let Some(rest) = v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")) {
            let t = rest.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": "unauthorized",
            "hint": "pass X-Acend-Key header (or ?key= for WebSocket)"
        })),
    )
        .into_response()
}

/// Axum extractor: rejects the request if API key is configured and missing/wrong.
pub struct RequireApiKey;

impl FromRequestParts<crate::AppState> for RequireApiKey {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &crate::AppState,
    ) -> Result<Self, Self::Rejection> {
        let query_key = parts.uri.query().and_then(|q| {
            q.split('&').find_map(|pair| {
                let mut it = pair.splitn(2, '=');
                let k = it.next()?;
                let v = it.next().unwrap_or("");
                if k == "key" {
                    Some(urlencoding_decode(v))
                } else {
                    None
                }
            })
        });
        let provided = extract_api_key(&parts.headers, query_key.as_deref());
        if state.auth.check_key(provided.as_deref()) {
            Ok(RequireApiKey)
        } else {
            Err(unauthorized())
        }
    }
}

fn urlencoding_decode(s: &str) -> String {
    // Minimal decode for keys (alphanumeric + - _). Full URL decode not required.
    s.replace("%2D", "-")
        .replace("%5F", "_")
        .replace('+', " ")
}
