//! API authentication: HS256 JWT verification and an Axum middleware.
//!
//! Only the symmetric `HS256` algorithm is supported, matching the shared
//! `MEDIAFORGE_JWT_SECRET`. The verifier explicitly rejects any other `alg`
//! (including `none`) to avoid algorithm-confusion attacks, and compares the
//! signature in constant time. Verification is hand-rolled on top of the same
//! `hmac`/`sha2`/`base64` primitives used for link signing, keeping the
//! dependency surface small.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use mediaforge_config::AuthConfig;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("authorization header missing or not a bearer token")]
    MissingBearerToken,
    #[error("malformed JWT")]
    MalformedToken,
    #[error("unsupported JWT alg (only HS256 is accepted)")]
    UnsupportedAlgorithm,
    #[error("invalid JWT signature")]
    InvalidSignature,
    #[error("JWT has expired")]
    Expired,
    #[error("JWT is not yet valid")]
    NotYetValid,
    #[error("authentication is enabled but no signing secret is configured")]
    Misconfigured,
}

/// Registered JWT claims this service understands. Custom claims are ignored.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Claims {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iat: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct JwtHeader {
    alg: String,
}

/// Verifies an HS256 JWT against `secret`, validating `exp`/`nbf` (with
/// `leeway_seconds` of clock tolerance) against `now_unix`. Returns the claims.
pub fn verify_hs256(
    token: &str,
    secret: &str,
    leeway_seconds: u64,
    now_unix: i64,
) -> Result<Claims, AuthError> {
    let mut parts = token.split('.');
    let (header_b64, payload_b64, signature_b64) =
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(header), Some(payload), Some(signature), None) => (header, payload, signature),
            _ => return Err(AuthError::MalformedToken),
        };

    let header_bytes = decode_segment(header_b64)?;
    let header: JwtHeader =
        serde_json::from_slice(&header_bytes).map_err(|_| AuthError::MalformedToken)?;
    if header.alg != "HS256" {
        return Err(AuthError::UnsupportedAlgorithm);
    }

    let signing_input = format!("{header_b64}.{payload_b64}");
    let expected = sign_input(signing_input.as_bytes(), secret);
    let provided = decode_segment(signature_b64)?;
    if !constant_time_eq(&expected, &provided) {
        return Err(AuthError::InvalidSignature);
    }

    let payload_bytes = decode_segment(payload_b64)?;
    let claims: Claims =
        serde_json::from_slice(&payload_bytes).map_err(|_| AuthError::MalformedToken)?;

    let leeway = leeway_seconds as i64;
    if let Some(exp) = claims.exp {
        if now_unix > exp + leeway {
            return Err(AuthError::Expired);
        }
    }
    if let Some(nbf) = claims.nbf {
        if now_unix + leeway < nbf {
            return Err(AuthError::NotYetValid);
        }
    }

    Ok(claims)
}

/// Mints an HS256 JWT for the given claims. Useful for tooling and tests.
pub fn encode_hs256(claims: &Claims, secret: &str) -> String {
    let header = serde_json::json!({"alg": "HS256", "typ": "JWT"});
    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("header serializes"));
    let payload_b64 =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signing_input = format!("{header_b64}.{payload_b64}");
    let signature = URL_SAFE_NO_PAD.encode(sign_input(signing_input.as_bytes(), secret));
    format!("{signing_input}.{signature}")
}

/// Axum middleware that enforces authentication when enabled. Attach via
/// `axum::middleware::from_fn_with_state(Arc::new(auth_config), require_auth)`.
/// When auth is disabled the request passes through unchanged; `/health` is
/// always reachable so liveness probes work regardless of auth.
pub async fn require_auth(
    State(config): State<Arc<AuthConfig>>,
    request: Request,
    next: Next,
) -> Response {
    if !config.enabled || request.uri().path() == "/health" {
        return next.run(request).await;
    }

    let Some(secret) = config.jwt_secret.as_deref() else {
        return AuthError::Misconfigured.into_response();
    };

    let token = match bearer_token(&request) {
        Some(token) => token,
        None => return AuthError::MissingBearerToken.into_response(),
    };

    match verify_hs256(&token, secret, config.leeway_seconds, current_unix_time()) {
        Ok(_claims) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let status = match self {
            AuthError::Misconfigured => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::UNAUTHORIZED,
        };
        let body = Json(serde_json::json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}

fn bearer_token(request: &Request<Body>) -> Option<String> {
    let value = request.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))?;
    let token = token.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

fn sign_input(signing_input: &[u8], secret: &str) -> Vec<u8> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(signing_input);
    mac.finalize().into_bytes().to_vec()
}

fn decode_segment(segment: &str) -> Result<Vec<u8>, AuthError> {
    // JWT uses base64url without padding, but tolerate trailing '=' just in case.
    let trimmed = segment.trim_end_matches('=');
    URL_SAFE_NO_PAD
        .decode(trimmed)
        .map_err(|_| AuthError::MalformedToken)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

fn current_unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "test-secret";

    fn claims_with_exp(exp: i64) -> Claims {
        Claims {
            sub: Some("user-1".to_string()),
            exp: Some(exp),
            ..Default::default()
        }
    }

    #[test]
    fn valid_token_round_trips() {
        let token = encode_hs256(&claims_with_exp(1_000), SECRET);
        let claims = verify_hs256(&token, SECRET, 30, 500).unwrap();
        assert_eq!(claims.sub.as_deref(), Some("user-1"));
    }

    #[test]
    fn expired_token_is_rejected() {
        let token = encode_hs256(&claims_with_exp(1_000), SECRET);
        let error = verify_hs256(&token, SECRET, 30, 2_000).unwrap_err();
        assert_eq!(error, AuthError::Expired);
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let token = encode_hs256(&claims_with_exp(1_000), SECRET);
        let error = verify_hs256(&token, "other-secret", 30, 500).unwrap_err();
        assert_eq!(error, AuthError::InvalidSignature);
    }

    #[test]
    fn alg_none_is_rejected() {
        // Forge a token with alg=none and an empty signature.
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"attacker"}"#);
        let forged = format!("{header}.{payload}.");
        let error = verify_hs256(&forged, SECRET, 30, 500).unwrap_err();
        assert_eq!(error, AuthError::UnsupportedAlgorithm);
    }

    #[test]
    fn not_yet_valid_token_is_rejected() {
        let claims = Claims {
            nbf: Some(1_000),
            ..Default::default()
        };
        let token = encode_hs256(&claims, SECRET);
        let error = verify_hs256(&token, SECRET, 30, 500).unwrap_err();
        assert_eq!(error, AuthError::NotYetValid);
    }

    #[test]
    fn leeway_allows_recently_expired_token() {
        let token = encode_hs256(&claims_with_exp(1_000), SECRET);
        // 10s past expiry but within 30s leeway.
        assert!(verify_hs256(&token, SECRET, 30, 1_010).is_ok());
    }
}
