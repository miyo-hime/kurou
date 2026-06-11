use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{Form, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

pub struct OAuthStore {
    pending: Mutex<HashMap<String, PendingCode>>,
    access_token: String,
    base_url: String,
}

struct PendingCode {
    code_challenge: String,
    redirect_uri: String,
    created_at: Instant,
}

impl OAuthStore {
    pub fn new(access_token: String, base_url: String) -> Arc<Self> {
        Arc::new(Self {
            pending: Mutex::new(HashMap::new()),
            access_token,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }
}

pub async fn metadata(State(store): State<Arc<OAuthStore>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": store.base_url,
        "authorization_endpoint": format!("{}/authorize", store.base_url),
        "token_endpoint": format!("{}/token", store.base_url),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "grant_types_supported": ["authorization_code"],
    }))
}

pub async fn protected_resource(State(store): State<Arc<OAuthStore>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "resource": store.base_url,
        "authorization_servers": [store.base_url],
        "bearer_methods_supported": ["header"],
    }))
}

#[derive(Deserialize)]
pub struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: String,
    state: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
}

pub async fn authorize_get(Query(params): Query<AuthorizeQuery>) -> Response {
    if params.response_type != "code" || params.code_challenge_method != "S256" {
        return (StatusCode::BAD_REQUEST, "unsupported OAuth request").into_response();
    }

    let state = params.state.as_deref().unwrap_or("");
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>kurou</title>
  <style>
    * {{ box-sizing: border-box; margin: 0; padding: 0; }}
    body {{ min-height: 100vh; display: grid; place-items: center; background: #101114; color: #eceff4; font-family: ui-monospace, SFMono-Regular, Consolas, monospace; }}
    .card {{ width: min(92vw, 390px); padding: 2rem; border: 1px solid #2f3542; border-radius: 8px; background: #181b20; }}
    h1 {{ font-size: 1rem; margin-bottom: 0.45rem; }}
    p {{ color: #aab2c0; font-size: 0.85rem; margin-bottom: 1.4rem; line-height: 1.45; }}
    button {{ width: 100%; border: 0; border-radius: 4px; padding: 0.75rem 1rem; background: #4f46e5; color: white; font: inherit; cursor: pointer; }}
    button:hover {{ background: #6366f1; }}
  </style>
</head>
<body>
  <main class="card">
    <h1>kurou</h1>
    <p>allow this client to use the configured kurou bearer token?</p>
    <form method="POST" action="/authorize">
      <input type="hidden" name="code_challenge" value="{code_challenge}">
      <input type="hidden" name="redirect_uri" value="{redirect_uri}">
      <input type="hidden" name="client_id" value="{client_id}">
      <input type="hidden" name="state" value="{state}">
      <button type="submit">allow</button>
    </form>
  </main>
</body>
</html>"#,
        code_challenge = html_escape(&params.code_challenge),
        redirect_uri = html_escape(&params.redirect_uri),
        client_id = html_escape(&params.client_id),
        state = html_escape(state),
    );

    Html(html).into_response()
}

#[derive(Deserialize)]
pub struct AuthorizeForm {
    code_challenge: String,
    redirect_uri: String,
    #[allow(dead_code)]
    client_id: String,
    state: Option<String>,
}

pub async fn authorize_post(
    State(store): State<Arc<OAuthStore>>,
    Form(params): Form<AuthorizeForm>,
) -> Response {
    let code = Uuid::new_v4().to_string();

    {
        let mut pending = store.pending.lock().unwrap();
        pending.retain(|_, code| code.created_at.elapsed() < Duration::from_secs(300));
        pending.insert(
            code.clone(),
            PendingCode {
                code_challenge: params.code_challenge,
                redirect_uri: params.redirect_uri.clone(),
                created_at: Instant::now(),
            },
        );
    }

    let Ok(mut redirect) = Url::parse(&params.redirect_uri) else {
        return (StatusCode::BAD_REQUEST, "invalid redirect_uri").into_response();
    };
    {
        let mut query = redirect.query_pairs_mut();
        query.append_pair("code", &code);
        if let Some(state) = &params.state {
            query.append_pair("state", state);
        }
    }

    Redirect::to(redirect.as_str()).into_response()
}

#[derive(Deserialize)]
pub struct TokenForm {
    grant_type: String,
    code: String,
    code_verifier: String,
    redirect_uri: String,
    #[allow(dead_code)]
    client_id: Option<String>,
}

pub async fn token(
    State(store): State<Arc<OAuthStore>>,
    Form(params): Form<TokenForm>,
) -> Response {
    if params.grant_type != "authorization_code" {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "unsupported_grant_type"})),
        )
            .into_response();
    }

    let pending = store.pending.lock().unwrap().remove(&params.code);
    let Some(pending) = pending else {
        return invalid_grant();
    };

    if pending.created_at.elapsed() > Duration::from_secs(300)
        || pending.redirect_uri != params.redirect_uri
    {
        return invalid_grant();
    }

    // tiny oauth cosplay, but the pkce bit is real.
    let hash = Sha256::digest(params.code_verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(hash);

    if computed != pending.code_challenge {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_grant",
                "error_description": "pkce verification failed",
            })),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "access_token": store.access_token,
        "token_type": "Bearer",
    }))
    .into_response()
}

fn invalid_grant() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"error": "invalid_grant"})),
    )
        .into_response()
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::html_escape;

    #[test]
    fn escapes_hidden_form_values() {
        assert_eq!(
            html_escape(r#"<tag a="b">&"#),
            "&lt;tag a=&quot;b&quot;&gt;&amp;"
        );
    }
}
