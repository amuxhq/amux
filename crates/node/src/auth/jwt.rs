//! JWT validation for cloud server mode.
//!
//! Validates connection tokens using JWKS from the cloud service. The cloud
//! signs them RS256 and nothing else, so that is the one algorithm accepted:
//! a token's header never chooses how it is checked.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Client;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::Clock;
use crate::auth::claims::{ConnectionClaims, Tier};

/// How long to cache JWKS keys before re-fetching.
const JWKS_CACHE_TTL: Duration = Duration::from_secs(3600);
/// How long a key set fetch may take: a slow cloud must not hold up every
/// connection waiting on it.
const JWKS_FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The audience the cloud mints connection tokens for.
const AUDIENCE: &str = "amux_token";
/// How long past its expiry a token is still taken, for clocks that differ.
const LEEWAY_SECS: u64 = 60;

#[derive(Debug, Error)]
pub enum JwtError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Malformed token")]
    Malformed,

    #[error("Token signed with {0}, not RS256")]
    Algorithm(String),

    #[error("Missing key ID in token header")]
    MissingKeyId,

    #[error("Unknown signing key")]
    UnknownKey,

    #[error("Token signature does not verify")]
    Signature,

    #[error("Token is not for this audience")]
    Audience,

    #[error("Token was not issued by the cloud")]
    Issuer,

    #[error("Token has expired")]
    Expired,

    #[error("Token host/port mismatch")]
    HostMismatch,

    #[error("Missing client_id in token")]
    MissingClientId,

    #[error("Missing or unrecognised tier in token")]
    MissingTier,
}

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    kid: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawConnectionClaims {
    sub: String,
    client_id: String,
    host: String,
    port: u16,
    exp: u64,
    iss: String,
    aud: Audience,
    tier: Option<serde_json::Value>,
}

/// A JWT's `aud`: one audience, or a list of them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn names(&self, audience: &str) -> bool {
        match self {
            Self::One(one) => one == audience,
            Self::Many(many) => many.iter().any(|one| one == audience),
        }
    }
}

/// JWKS key set structure
#[derive(Debug, Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

/// Individual JWK key
#[derive(Debug, Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

/// An RSA public key, as its big-endian modulus and exponent.
struct RsaKey {
    n: Vec<u8>,
    e: Vec<u8>,
}

/// JWT validator with JWKS caching
pub struct JwtValidator {
    jwks_url: String,
    issuer: String,
    http_client: Client,
    keys: Arc<RwLock<HashMap<String, RsaKey>>>,
    last_fetch: Arc<RwLock<Option<Instant>>>,
    clock: Arc<dyn Clock>,
}

impl JwtValidator {
    pub(crate) fn new_with_clock(cloud_url: &str, clock: Arc<dyn Clock>) -> Self {
        let issuer = cloud_url.trim_end_matches('/').to_owned();
        Self {
            jwks_url: format!("{issuer}/.well-known/openid-configuration/jwks"),
            issuer,
            http_client: Client::builder()
                .timeout(JWKS_FETCH_TIMEOUT)
                .build()
                .unwrap_or_default(),
            keys: Arc::new(RwLock::new(HashMap::new())),
            last_fetch: Arc::new(RwLock::new(None)),
            clock,
        }
    }

    /// Validate a connection token
    pub async fn validate(
        &self,
        token: &str,
        expected_host: &str,
        expected_port: u16,
    ) -> Result<ConnectionClaims, JwtError> {
        // Ensure JWKS is fresh (cache for 1 hour)
        self.ensure_jwks_fresh().await?;

        let mut parts = token.split('.');
        let (Some(header), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(JwtError::Malformed);
        };
        let signed = &token[..header.len() + 1 + payload.len()];
        let header: Header = decode_part(header)?;
        if header.alg != "RS256" {
            return Err(JwtError::Algorithm(header.alg));
        }
        let kid = header.kid.ok_or(JwtError::MissingKeyId)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| JwtError::Malformed)?;
        {
            let keys = self.keys.read().await;
            let key = keys.get(&kid).ok_or(JwtError::UnknownKey)?;
            RsaPublicKeyComponents {
                n: &key.n,
                e: &key.e,
            }
            .verify(&RSA_PKCS1_2048_8192_SHA256, signed.as_bytes(), &signature)
            .map_err(|_| JwtError::Signature)?;
        }

        let raw: RawConnectionClaims = decode_part(payload)?;
        if !raw.aud.names(AUDIENCE) {
            return Err(JwtError::Audience);
        }
        if raw.iss != self.issuer {
            return Err(JwtError::Issuer);
        }
        // The policy clock, not the process wall clock, so test issuers and
        // this verifier share one explicitly driven clock.
        let now = (self.clock.now_ms().max(0) / 1000) as u64;
        if raw.exp < now.saturating_sub(LEEWAY_SECS) {
            return Err(JwtError::Expired);
        }
        let tier = match raw.tier.as_ref().and_then(serde_json::Value::as_str) {
            Some("free") => Tier::Free,
            Some("pro") => Tier::Pro,
            _ => return Err(JwtError::MissingTier),
        };
        let claims = ConnectionClaims {
            sub: raw.sub,
            client_id: raw.client_id,
            host: raw.host,
            port: raw.port,
            exp: raw.exp,
            tier,
        };

        if claims.client_id.is_empty() {
            return Err(JwtError::MissingClientId);
        }

        // Verify host/port match
        if claims.host != expected_host || claims.port != expected_port {
            tracing::warn!(token_host = %claims.host, token_port = claims.port, expected_host, expected_port, "token host/port mismatch");
            return Err(JwtError::HostMismatch);
        }

        Ok(claims)
    }

    async fn ensure_jwks_fresh(&self) -> Result<(), JwtError> {
        if self.is_cache_fresh().await {
            return Ok(());
        }
        self.refresh_jwks().await
    }

    async fn is_cache_fresh(&self) -> bool {
        matches!(*self.last_fetch.read().await, Some(t) if t.elapsed() < JWKS_CACHE_TTL)
    }

    async fn refresh_jwks(&self) -> Result<(), JwtError> {
        tracing::debug!("fetching JWKS");
        let response = self.http_client.get(&self.jwks_url).send().await?;
        let jwks: JwkSet = response.json().await?;

        let mut keys = self.keys.write().await;
        keys.clear();

        for jwk in jwks.keys {
            if let (Some(kid), Some(n), Some(e)) = (jwk.kid, jwk.n, jwk.e)
                && jwk.kty == "RSA"
                && let (Ok(n), Ok(e)) = (URL_SAFE_NO_PAD.decode(n), URL_SAFE_NO_PAD.decode(e))
            {
                keys.insert(kid, RsaKey { n, e });
            }
        }

        *self.last_fetch.write().await = Some(Instant::now());
        Ok(())
    }
}

/// One base64url part of a token, read as JSON.
fn decode_part<T: serde::de::DeserializeOwned>(part: &str) -> Result<T, JwtError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| JwtError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| JwtError::Malformed)
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::STANDARD;
    use ring::rand::SystemRandom;
    use ring::signature::{KeyPair as _, RSA_PKCS1_SHA256, RsaKeyPair};

    use super::*;

    const CLOUD: &str = "http://cloud.test";
    const FAR_FUTURE: u64 = 4_102_444_800;

    include!("test_signing_key.rs");

    fn signing_key() -> RsaKeyPair {
        let body: String = CLOUD_SIGNING_KEY
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        RsaKeyPair::from_pkcs8(&STANDARD.decode(body).unwrap()).unwrap()
    }

    async fn validator(clock: Arc<dyn Clock>) -> JwtValidator {
        let validator = JwtValidator::new_with_clock(CLOUD, clock);
        let public: RsaPublicKeyComponents<Vec<u8>> = signing_key().public_key().into();
        validator.keys.write().await.insert(
            "test-key".to_string(),
            RsaKey {
                n: public.n,
                e: public.e,
            },
        );
        *validator.last_fetch.write().await = Some(Instant::now());
        validator
    }

    /// A token with `header` over `claims`, signed with the test key.
    fn signed(header: serde_json::Value, claims: serde_json::Value) -> String {
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let key = signing_key();
        let mut signature = vec![0; key.public().modulus_len()];
        key.sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            input.as_bytes(),
            &mut signature,
        )
        .unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    fn claims(exp: u64) -> serde_json::Value {
        serde_json::json!({
            "sub": "00000000-0000-0000-0000-000000000001",
            "client_id": "cli",
            "host": "relay",
            "port": 9443,
            "exp": exp,
            "iss": CLOUD,
            "aud": AUDIENCE,
            "tier": "pro",
        })
    }

    fn token(claims: serde_json::Value) -> String {
        signed(
            serde_json::json!({"alg": "RS256", "kid": "test-key"}),
            claims,
        )
    }

    fn with(
        mut claims: serde_json::Value,
        key: &str,
        value: serde_json::Value,
    ) -> serde_json::Value {
        if value.is_null() {
            claims.as_object_mut().unwrap().remove(key);
        } else {
            claims[key] = value;
        }
        claims
    }

    #[tokio::test]
    async fn validator_requires_expected_audience_claim() {
        let validator = validator(Arc::new(agent_dir::SystemClock)).await;

        for aud in [serde_json::Value::Null, serde_json::json!("other")] {
            let token = token(with(claims(FAR_FUTURE), "aud", aud));
            assert!(validator.validate(&token, "relay", 9443).await.is_err());
        }
        let listed = token(with(
            claims(FAR_FUTURE),
            "aud",
            serde_json::json!(["other", AUDIENCE]),
        ));
        assert!(validator.validate(&listed, "relay", 9443).await.is_ok());
        assert!(
            validator
                .validate(&token(claims(FAR_FUTURE)), "relay", 9443)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn validator_requires_the_cloud_as_issuer() {
        let validator = validator(Arc::new(agent_dir::SystemClock)).await;

        let other = token(with(
            claims(FAR_FUTURE),
            "iss",
            serde_json::json!("http://elsewhere.test"),
        ));
        assert!(matches!(
            validator.validate(&other, "relay", 9443).await,
            Err(JwtError::Issuer)
        ));
        let missing = token(with(claims(FAR_FUTURE), "iss", serde_json::Value::Null));
        assert!(matches!(
            validator.validate(&missing, "relay", 9443).await,
            Err(JwtError::Malformed)
        ));
    }

    #[tokio::test]
    async fn validator_accepts_only_rs256_signed_by_a_known_key() {
        let validator = validator(Arc::new(agent_dir::SystemClock)).await;
        let good = token(claims(FAR_FUTURE));

        for alg in ["HS256", "none", "RS512", "PS256"] {
            let other = signed(
                serde_json::json!({"alg": alg, "kid": "test-key"}),
                claims(FAR_FUTURE),
            );
            assert!(matches!(
                validator.validate(&other, "relay", 9443).await,
                Err(JwtError::Algorithm(named)) if named == alg
            ));
        }
        let unknown = signed(
            serde_json::json!({"alg": "RS256", "kid": "another-key"}),
            claims(FAR_FUTURE),
        );
        assert!(matches!(
            validator.validate(&unknown, "relay", 9443).await,
            Err(JwtError::UnknownKey)
        ));
        // The same signature over different claims.
        let (signed_part, signature) = good.rsplit_once('.').unwrap();
        let (header, _) = signed_part.split_once('.').unwrap();
        let altered = format!(
            "{header}.{}.{signature}",
            URL_SAFE_NO_PAD.encode(with(claims(FAR_FUTURE), "tier", "free".into()).to_string())
        );
        assert!(matches!(
            validator.validate(&altered, "relay", 9443).await,
            Err(JwtError::Signature)
        ));
        assert!(matches!(
            validator.validate("not-a-token", "relay", 9443).await,
            Err(JwtError::Malformed)
        ));
    }

    #[tokio::test]
    async fn validator_requires_a_recognised_tier_claim() {
        let validator = validator(Arc::new(agent_dir::SystemClock)).await;

        for tier in [serde_json::Value::Null, serde_json::json!("enterprise")] {
            let token = token(with(claims(FAR_FUTURE), "tier", tier));
            assert!(matches!(
                validator.validate(&token, "relay", 9443).await,
                Err(JwtError::MissingTier)
            ));
        }

        let free = token(with(claims(FAR_FUTURE), "tier", "free".into()));
        let claims = validator.validate(&free, "relay", 9443).await.unwrap();
        assert_eq!(claims.tier, Tier::Free);
    }

    #[tokio::test]
    async fn expiration_uses_the_injected_policy_clock() {
        let epoch = 1_700_000_000;
        let clock = Arc::new(agent_dir::ManualClock::new(epoch as i64 * 1000));
        let validator = validator(clock.clone()).await;
        let token = token(claims(epoch + 100));

        assert!(validator.validate(&token, "relay", 9443).await.is_ok());
        clock.advance(160_000);
        assert!(validator.validate(&token, "relay", 9443).await.is_ok());
        clock.advance(1_000);
        assert!(matches!(
            validator.validate(&token, "relay", 9443).await,
            Err(JwtError::Expired)
        ));
    }
}
