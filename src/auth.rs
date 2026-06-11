use std::sync::Arc;

use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct AuthConfig {
    public_base_url: String,
    tokens: Arc<Vec<AuthToken>>,
}

#[derive(Clone, Debug)]
struct AuthToken {
    label: String,
    token: String,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ClientIdentity(pub String);

impl AuthConfig {
    pub fn new(public_base_url: String, raw_tokens: &[String]) -> Self {
        let tokens = raw_tokens
            .iter()
            .enumerate()
            .filter_map(|(index, raw)| parse_token(index, raw))
            .collect();

        Self {
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
            tokens: Arc::new(tokens),
        }
    }

    pub fn is_enabled(&self) -> bool {
        !self.tokens.is_empty()
    }

    pub fn token_count(&self) -> usize {
        self.tokens.len()
    }

    pub fn token_for_label(&self, label: &str) -> Option<String> {
        self.tokens
            .iter()
            .find(|entry| entry.label == label)
            .map(|entry| entry.token.clone())
    }

    pub fn default_token(&self) -> Option<String> {
        self.tokens.first().map(|entry| entry.token.clone())
    }

    pub fn oauth_token(&self, label: Option<&str>) -> Option<String> {
        match label {
            Some(label) => self.token_for_label(label),
            None => self.default_token(),
        }
    }

    fn resolve(&self, candidate: &str) -> Option<String> {
        self.tokens
            .iter()
            .find(|entry| entry.token == candidate)
            .map(|entry| entry.label.clone())
    }

    fn unauthorized_response(&self) -> Response {
        let www_auth = format!(
            r#"Bearer realm="kurou", resource_metadata="{}/.well-known/oauth-protected-resource""#,
            self.public_base_url
        );

        match HeaderValue::from_str(&www_auth) {
            Ok(value) => {
                let mut response = StatusCode::UNAUTHORIZED.into_response();
                response.headers_mut().insert("www-authenticate", value);
                response
            }
            Err(error) => {
                tracing::warn!(%error, "failed to build www-authenticate header");
                StatusCode::UNAUTHORIZED.into_response()
            }
        }
    }

    fn protected_resource_metadata(&self) -> serde_json::Value {
        json!({
            "resource": self.public_base_url,
            "bearer_methods_supported": ["header"],
        })
    }
}

fn parse_token(index: usize, raw: &str) -> Option<AuthToken> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let (label, token) = raw
        .split_once(':')
        .map(|(label, token)| (label.trim(), token.trim()))
        .unwrap_or_else(|| ("token", raw));

    if token.is_empty() {
        return None;
    }

    let label = if label.is_empty() || label == "token" {
        format!("token-{}", index + 1)
    } else {
        label.to_string()
    };

    Some(AuthToken {
        label,
        token: token.to_string(),
    })
}

pub async fn auth_middleware(auth: Arc<AuthConfig>, mut request: Request, next: Next) -> Response {
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match bearer.and_then(|token| auth.resolve(token.trim())) {
        Some(identity) => {
            tracing::debug!(identity, "authenticated http mcp request");
            request.extensions_mut().insert(ClientIdentity(identity));
            next.run(request).await
        }
        None => auth.unauthorized_response(),
    }
}

pub async fn protected_resource(State(auth): State<Arc<AuthConfig>>) -> Json<serde_json::Value> {
    Json(auth.protected_resource_metadata())
}

#[cfg(test)]
mod tests {
    use super::AuthConfig;

    #[test]
    fn auth_tokens_support_labels_and_bare_values() {
        let auth = AuthConfig::new(
            "https://kurou.example".to_string(),
            &[
                "koma:koma-token".to_string(),
                "guest-token".to_string(),
                "empty:".to_string(),
            ],
        );

        assert_eq!(auth.token_count(), 2);
        assert_eq!(auth.resolve("koma-token").as_deref(), Some("koma"));
        assert_eq!(auth.resolve("guest-token").as_deref(), Some("token-2"));
        assert_eq!(auth.resolve("nope"), None);
        assert_eq!(auth.token_for_label("koma").as_deref(), Some("koma-token"));
        assert_eq!(auth.default_token().as_deref(), Some("koma-token"));
        assert_eq!(
            auth.oauth_token(Some("token-2")).as_deref(),
            Some("guest-token")
        );
        assert_eq!(auth.oauth_token(Some("missing")), None);
        assert_eq!(auth.oauth_token(None).as_deref(), Some("koma-token"));
    }

    #[test]
    fn public_base_url_is_trimmed_for_metadata() {
        let auth = AuthConfig::new("https://kurou.example/".to_string(), &[]);
        let metadata = auth.protected_resource_metadata();

        assert_eq!(metadata["resource"], "https://kurou.example");
        assert!(metadata.get("authorization_servers").is_none());
    }
}
