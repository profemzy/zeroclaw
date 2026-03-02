//! JWKS-based JWT validation for Keycloak OIDC tokens.
//!
//! Adapted from LedgerForge `src/services/oidc.rs` for the ZeroClaw gateway.
//! Validates RS256 tokens, extracts claims (sub, email, business_id, roles),
//! and caches JWKS keys with rate-limited refresh.

use anyhow::{bail, Context, Result};
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Minimum interval between JWKS refreshes (5 minutes).
const JWKS_REFRESH_INTERVAL_SECS: u64 = 300;

/// OIDC token validation service.
/// Fetches JWKS from Keycloak and validates RS256 tokens.
///
/// The JWKS cache is shared across clones via `Arc`, so Axum's
/// per-request state cloning does not discard cached keys.
#[derive(Debug, Clone)]
pub struct OidcService {
    jwks_url: String,
    issuer: String,
    cached_jwks: Arc<RwLock<Option<JwkSet>>>,
    last_refresh: Arc<RwLock<Instant>>,
    http: reqwest::Client,
}

/// Claims from a Keycloak-issued RS256 JWT token.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct KeycloakClaims {
    /// Keycloak user UUID (subject)
    pub sub: String,
    pub preferred_username: Option<String>,
    pub email: Option<String>,
    pub realm_access: Option<RealmAccess>,
    /// Custom claim injected via Keycloak protocol mapper
    pub business_id: Option<String>,
    pub exp: i64,
    pub iat: i64,
    pub iss: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RealmAccess {
    pub roles: Vec<String>,
}

impl OidcService {
    /// Create a new OidcService.
    ///
    /// `keycloak_url` — base URL for JWKS fetching (e.g. `https://auth.dev.oluto.app`)
    /// `realm`        — Keycloak realm name (e.g. `oluto`)
    /// `issuer_url`   — optional public URL for token issuer validation.
    ///                   If None, `keycloak_url` is used for both.
    /// `http`         — shared reqwest client (reused from gateway)
    pub fn new(
        keycloak_url: &str,
        realm: &str,
        issuer_url: Option<&str>,
        http: reqwest::Client,
    ) -> Self {
        let base = keycloak_url.trim_end_matches('/');
        let issuer_base = issuer_url
            .map(|u| u.trim_end_matches('/').to_string())
            .unwrap_or_else(|| base.to_string());
        let issuer = format!("{issuer_base}/realms/{realm}");
        let jwks_url = format!("{base}/realms/{realm}/protocol/openid-connect/certs");

        tracing::info!("OIDC service enabled — issuer: {issuer}");

        Self {
            jwks_url,
            issuer,
            cached_jwks: Arc::new(RwLock::new(None)),
            last_refresh: Arc::new(RwLock::new(
                Instant::now() - std::time::Duration::from_secs(JWKS_REFRESH_INTERVAL_SECS),
            )),
            http,
        }
    }

    /// Validate a Keycloak RS256 JWT token.
    /// Returns the decoded claims on success.
    pub async fn validate_token(&self, token: &str) -> Result<KeycloakClaims> {
        let header = decode_header(token).context("invalid JWT header")?;

        if header.alg != Algorithm::RS256 {
            bail!("unsupported JWT algorithm {:?}, expected RS256", header.alg);
        }

        let kid = header
            .kid
            .as_deref()
            .context("JWT header missing kid")?;

        let decoding_key = self.get_decoding_key(kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.issuer]);
        // Keycloak tokens don't always have an `aud` claim matching a single value,
        // so we skip audience validation. The issuer check is sufficient
        // since we only trust our own Keycloak realm.
        validation.validate_aud = false;

        let token_data = decode::<KeycloakClaims>(token, &decoding_key, &validation)
            .context("JWT validation failed")?;

        Ok(token_data.claims)
    }

    /// Get the DecodingKey for the given kid, refreshing JWKS if needed.
    async fn get_decoding_key(&self, kid: &str) -> Result<DecodingKey> {
        // Try cached JWKS first
        {
            let jwks = self.cached_jwks.read().await;
            if let Some(ref jwks) = *jwks {
                if let Some(jwk) = jwks.find(kid) {
                    return DecodingKey::from_jwk(jwk)
                        .context("failed to create DecodingKey from cached JWK");
                }
            }
        }

        // Key not found in cache — refresh JWKS (with rate limiting)
        self.refresh_jwks().await?;

        // Try again after refresh
        let jwks = self.cached_jwks.read().await;
        let jwks = jwks.as_ref().context("JWKS cache empty after refresh")?;
        let jwk = jwks
            .find(kid)
            .with_context(|| format!("key id '{kid}' not found in JWKS after refresh"))?;

        DecodingKey::from_jwk(jwk).context("failed to create DecodingKey from JWK")
    }

    /// Fetch JWKS from Keycloak, rate-limited to avoid hammering the endpoint.
    async fn refresh_jwks(&self) -> Result<()> {
        {
            let last = self.last_refresh.read().await;
            if last.elapsed().as_secs() < JWKS_REFRESH_INTERVAL_SECS {
                let has_keys = self.cached_jwks.read().await.is_some();
                if has_keys {
                    return Ok(());
                }
            }
        }

        tracing::debug!("Refreshing JWKS from {}", self.jwks_url);

        let jwks: JwkSet = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .context("JWKS fetch failed")?
            .json()
            .await
            .context("JWKS parse failed")?;

        tracing::info!("Loaded {} keys from JWKS endpoint", jwks.keys.len());

        *self.cached_jwks.write().await = Some(jwks);
        *self.last_refresh.write().await = Instant::now();

        Ok(())
    }
}

/// Map Keycloak `realm_access.roles` to the highest Oluto role.
/// Priority: "admin" > "accountant" > "viewer".
/// Returns "viewer" if no recognized role is found.
pub fn resolve_role(claims: &KeycloakClaims) -> String {
    let roles = match claims.realm_access {
        Some(ref ra) => &ra.roles,
        None => return "viewer".to_string(),
    };

    if roles.iter().any(|r| r == "admin") {
        "admin".to_string()
    } else if roles.iter().any(|r| r == "accountant") {
        "accountant".to_string()
    } else {
        "viewer".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_role_admin() {
        let claims = KeycloakClaims {
            sub: "u1".into(),
            preferred_username: None,
            email: None,
            realm_access: Some(RealmAccess {
                roles: vec!["viewer".into(), "admin".into(), "accountant".into()],
            }),
            business_id: None,
            exp: 0,
            iat: 0,
            iss: String::new(),
        };
        assert_eq!(resolve_role(&claims), "admin");
    }

    #[test]
    fn test_resolve_role_accountant() {
        let claims = KeycloakClaims {
            sub: "u2".into(),
            preferred_username: None,
            email: None,
            realm_access: Some(RealmAccess {
                roles: vec!["viewer".into(), "accountant".into()],
            }),
            business_id: None,
            exp: 0,
            iat: 0,
            iss: String::new(),
        };
        assert_eq!(resolve_role(&claims), "accountant");
    }

    #[test]
    fn test_resolve_role_viewer_default() {
        let claims = KeycloakClaims {
            sub: "u3".into(),
            preferred_username: None,
            email: None,
            realm_access: Some(RealmAccess {
                roles: vec!["uma_authorization".into()],
            }),
            business_id: None,
            exp: 0,
            iat: 0,
            iss: String::new(),
        };
        assert_eq!(resolve_role(&claims), "viewer");
    }

    #[test]
    fn test_resolve_role_no_realm_access() {
        let claims = KeycloakClaims {
            sub: "u4".into(),
            preferred_username: None,
            email: None,
            realm_access: None,
            business_id: None,
            exp: 0,
            iat: 0,
            iss: String::new(),
        };
        assert_eq!(resolve_role(&claims), "viewer");
    }

    #[test]
    fn test_deserialize_full_claims() {
        let json = serde_json::json!({
            "sub": "kc-user-001",
            "preferred_username": "janedoe",
            "email": "jane@example.com",
            "realm_access": { "roles": ["admin"] },
            "business_id": "aaaa-bbbb-cccc",
            "exp": 1999999999_i64,
            "iat": 1999999000_i64,
            "iss": "http://localhost:8080/realms/oluto"
        });

        let claims: KeycloakClaims = serde_json::from_value(json).unwrap();
        assert_eq!(claims.sub, "kc-user-001");
        assert_eq!(claims.preferred_username.as_deref(), Some("janedoe"));
        assert_eq!(claims.email.as_deref(), Some("jane@example.com"));
        assert_eq!(claims.business_id.as_deref(), Some("aaaa-bbbb-cccc"));
        assert_eq!(claims.realm_access.as_ref().unwrap().roles, vec!["admin"]);
    }

    #[test]
    fn test_deserialize_minimal_claims() {
        let json = serde_json::json!({
            "sub": "kc-user-002",
            "exp": 1999999999_i64,
            "iat": 1999999000_i64,
            "iss": "http://localhost:8080/realms/oluto"
        });

        let claims: KeycloakClaims = serde_json::from_value(json).unwrap();
        assert_eq!(claims.sub, "kc-user-002");
        assert!(claims.preferred_username.is_none());
        assert!(claims.email.is_none());
        assert!(claims.realm_access.is_none());
        assert!(claims.business_id.is_none());
    }
}
