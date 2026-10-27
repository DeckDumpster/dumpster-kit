//! Machine identity: verification of Pocket ID `client_credentials` bearer JWTs.
//!
//! A machine client (a monitor, a deploy hook) authenticates with a bearer token it obtained from
//! the issuer with its client secret. [`MachineVerifier::verify`] checks the signature against the
//! issuer's JWKS (cached, refreshed on an unknown `kid`), the issuer, the expiry and the audience,
//! then admits the token only if its client id is on the configured allow-list.
//!
//! The result is a [`MachineIdentity`]: bound to no tenant and a different type from
//! [`VerifiedIdentity`](crate::VerifiedIdentity), so nothing that resolves a tenant accepts one.
//! There is no conversion between them.

use std::collections::HashSet;
use std::sync::Arc;

use axum::http::StatusCode;
use jsonwebtoken::{Algorithm, Validation, decode};
use serde::Deserialize;

use crate::access::JwksCache;
use crate::error::Result as KitResult;
use crate::http::HttpError;

/// Startup configuration for machine-bearer verification.
pub struct MachineConfig {
    /// The issuer URL, matched exactly against `iss` (trailing `/` ignored).
    pub issuer: String,
    /// The audience this app answers to, e.g. its client id at the issuer.
    pub audience: String,
    /// The issuer's JWKS endpoint.
    pub jwks_url: String,
    /// Client ids admitted. An empty list admits nobody.
    pub allowed_clients: Vec<String>,
}

/// A verified machine client. Bound to no tenant; has no path to a `TenantId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineIdentity {
    client_id: String,
}

impl MachineIdentity {
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

#[derive(Deserialize)]
struct Claims {
    client_id: Option<String>,
    azp: Option<String>,
    sub: Option<String>,
}

/// Verifies machine bearer tokens against one issuer.
pub struct MachineVerifier {
    issuer: String,
    audience: String,
    allowed: HashSet<String>,
    cache: JwksCache,
}

impl MachineVerifier {
    /// Build the verifier and prime the JWKS cache; a failed fetch is a startup failure.
    pub async fn new(cfg: MachineConfig) -> KitResult<Arc<Self>> {
        let cache = JwksCache::new(cfg.jwks_url, reqwest::Client::new());
        cache.prime().await?;
        Ok(Arc::new(MachineVerifier {
            issuer: cfg.issuer.trim_end_matches('/').to_string(),
            audience: cfg.audience,
            allowed: cfg.allowed_clients.into_iter().collect(),
            cache,
        }))
    }

    /// Verify an `Authorization` header value (`Bearer <JWT>`).
    pub async fn verify_header(&self, authorization: &str) -> Result<MachineIdentity, HttpError> {
        let (scheme, token) = authorization
            .split_once(' ')
            .ok_or_else(|| HttpError::unauthorized("not a bearer credential"))?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(HttpError::unauthorized("not a bearer credential"));
        }
        self.verify(token.trim()).await
    }

    /// Verify a bare JWT.
    pub async fn verify(&self, token: &str) -> Result<MachineIdentity, HttpError> {
        let header = jsonwebtoken::decode_header(token)
            .map_err(|_| HttpError::unauthorized("invalid JWT"))?;
        if header.alg != Algorithm::RS256 {
            return Err(HttpError::unauthorized("invalid JWT"));
        }
        let kid = header
            .kid
            .ok_or_else(|| HttpError::unauthorized("JWT missing kid"))?;
        let key = self.cache.get(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.audience]);
        validation.set_issuer(&[&self.issuer]);
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| HttpError::unauthorized(format!("JWT rejected: {e}")))?;
        let c = data.claims;
        let client_id = [c.client_id, c.azp, c.sub]
            .into_iter()
            .flatten()
            .find(|v| !v.trim().is_empty())
            .ok_or_else(|| HttpError::unauthorized("JWT carries no client id"))?;
        if !self.allowed.contains(&client_id) {
            return Err(HttpError(
                StatusCode::FORBIDDEN,
                "client is not admitted".into(),
            ));
        }
        Ok(MachineIdentity { client_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::traits::PublicKeyParts;

    const ISS: &str = "https://id.example.test";
    const AUD: &str = "the-app";
    const KID: &str = "k1";

    struct Fx {
        v: Arc<MachineVerifier>,
        key: EncodingKey,
        other: EncodingKey,
    }

    fn keygen() -> (EncodingKey, String, String) {
        let sk = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let pk = sk.to_public_key();
        let pem = sk.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        (
            EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            URL_SAFE_NO_PAD.encode(pk.n().to_bytes_be()),
            URL_SAFE_NO_PAD.encode(pk.e().to_bytes_be()),
        )
    }

    async fn fx() -> Fx {
        let (key, n, e) = keygen();
        let (other, _, _) = keygen();
        let body = serde_json::json!({"keys":[{"kty":"RSA","kid":KID,"n":n,"e":e}]}).to_string();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/jwks",
                axum::routing::get(move || {
                    let b = body.clone();
                    async move {
                        axum::response::Response::builder()
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(b))
                            .unwrap()
                    }
                }),
            );
            axum::serve(l, app).await.ok();
        });
        let v = MachineVerifier::new(MachineConfig {
            issuer: ISS.into(),
            audience: AUD.into(),
            jwks_url: format!("http://127.0.0.1:{port}/jwks"),
            allowed_clients: vec!["monitor".into()],
        })
        .await
        .unwrap();
        Fx { v, key, other }
    }

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn mint(key: &EncodingKey, client: &str, aud: &str, exp_in: i64) -> String {
        let mut h = Header::new(Algorithm::RS256);
        h.kid = Some(KID.into());
        let claims = serde_json::json!({
            "iss": ISS, "aud": aud, "sub": client, "client_id": client,
            "iat": now(), "exp": now() + exp_in,
        });
        encode(&h, &claims, key).unwrap()
    }

    #[tokio::test]
    async fn allow_listed_client_is_admitted() {
        let f = fx().await;
        let t = mint(&f.key, "monitor", AUD, 600);
        let id = f.v.verify_header(&format!("Bearer {t}")).await.unwrap();
        assert_eq!(id.client_id(), "monitor");
    }

    #[tokio::test]
    async fn non_listed_client_is_refused() {
        let f = fx().await;
        let t = mint(&f.key, "stranger", AUD, 600);
        assert_eq!(f.v.verify(&t).await.unwrap_err().0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn wrong_audience_is_refused() {
        let f = fx().await;
        let t = mint(&f.key, "monitor", "other-app", 600);
        assert_eq!(
            f.v.verify(&t).await.unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn expired_is_refused() {
        let f = fx().await;
        let t = mint(&f.key, "monitor", AUD, -3600);
        assert_eq!(
            f.v.verify(&t).await.unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn bad_signature_is_refused() {
        let f = fx().await;
        let t = mint(&f.other, "monitor", AUD, 600);
        assert_eq!(
            f.v.verify(&t).await.unwrap_err().0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn non_bearer_scheme_is_refused() {
        let f = fx().await;
        let t = mint(&f.key, "monitor", AUD, 600);
        assert!(f.v.verify_header(&format!("Basic {t}")).await.is_err());
    }

    /// A machine identity must not become a tenant identity: no `Into` to `VerifiedIdentity`
    /// (the only way into `Tenants` resolution). The inherent method wins only when the bound
    /// holds, so adding such a conversion flips this test red.
    #[test]
    fn machine_identity_has_no_tenant_conversion() {
        use std::marker::PhantomData;
        struct Wrap<T>(PhantomData<T>);
        impl<T: Into<crate::VerifiedIdentity>> Wrap<T> {
            fn convertible() -> bool {
                true
            }
        }
        trait Fallback {
            fn convertible() -> bool {
                false
            }
        }
        impl<T> Fallback for Wrap<T> {}
        assert!(!Wrap::<MachineIdentity>::convertible());
        // Positive control: the probe does see a real conversion.
        struct Convertible;
        impl From<Convertible> for crate::VerifiedIdentity {
            fn from(_: Convertible) -> Self {
                crate::testing::identity("a@b.c")
            }
        }
        assert!(Wrap::<Convertible>::convertible());
    }
}
