//! Test infrastructure for the Access layer, for the kit's tests and a consumer's.
//!
//! [`TestAccessFixture`] spawns an in-process JWKS server on a loopback port, holds a real RSA
//! signing key, and mints tokens: a valid one, and one for every entry of the negative-JWT corpus
//! (`conformance/jwt/negative.json`) by name. There is no bypass: tests go through real RS256
//! verification.
//!
//! [`NEGATIVE_TOKENS`] is the list of names this module can mint. The kit's tests hold it equal to
//! the corpus, so a new attack cannot be added to one and not the other.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rsa::RsaPrivateKey;
use rsa::pkcs8::EncodePrivateKey;
use rsa::traits::PublicKeyParts;

use crate::access::{AccessConfig, AccessState, VerifiedIdentity};

/// Every negative token this module can mint, by corpus name.
pub const NEGATIVE_TOKENS: &[&str] = &[
    "garbage",
    "empty",
    "missing_kid",
    "unknown_kid",
    "expired",
    "not_yet_valid",
    "wrong_audience",
    "wrong_issuer",
    "tampered_payload",
    "alg_none",
    "alg_hs256_public_key",
    "alg_rs384",
    "no_exp",
    "no_email",
    "no_aud",
    "empty_email",
    "two_segments",
    "empty_signature",
];

struct TestKey {
    kid: String,
    encoding_key: EncodingKey,
    pem: String,
    n: String,
    e: String,
}

impl TestKey {
    fn generate(kid: impl Into<String>) -> Self {
        let mut rng = rsa::rand_core::OsRng;
        let sk = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let pk = sk.to_public_key();
        let n = URL_SAFE_NO_PAD.encode(pk.n().to_bytes_be());
        let e = URL_SAFE_NO_PAD.encode(pk.e().to_bytes_be());
        let pem = sk
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap()
            .to_string();
        TestKey {
            kid: kid.into(),
            encoding_key: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            pem,
            n,
            e,
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// An in-process JWKS server and RSA signing key.
///
/// `new()` binds a loopback port, serves the key set there, builds an [`AccessState`] against it
/// and returns the fixture. The server lives as long as the Tokio runtime that created it.
pub struct TestAccessFixture {
    pub access: Arc<AccessState>,
    pub aud: String,
    pub team_domain: String,
    pub jwks_url: String,
    key: TestKey,
}

impl TestAccessFixture {
    pub async fn new() -> Self {
        let key = TestKey::generate("test-kid-1");
        let aud = "test-aud-kit".to_string();
        let team_domain = "https://test.cloudflareaccess.com".to_string();

        let jwks_body = serde_json::json!({
            "keys": [{ "kty": "RSA", "kid": key.kid, "n": key.n, "e": key.e }]
        })
        .to_string();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/cdn-cgi/access/certs",
                axum::routing::get(move || {
                    let b = jwks_body.clone();
                    async move {
                        axum::response::Response::builder()
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(b))
                            .unwrap()
                    }
                }),
            );
            axum::serve(listener, app).await.ok();
        });

        let jwks_url = format!("http://127.0.0.1:{port}/cdn-cgi/access/certs");
        let access = AccessState::new(AccessConfig {
            team_domain: team_domain.clone(),
            aud: aud.clone(),
            jwks_url: jwks_url.clone(),
        })
        .await
        .unwrap();

        TestAccessFixture {
            access,
            aud,
            team_domain,
            jwks_url,
            key,
        }
    }

    fn claims(&self, email: &str, iss: &str, aud: &str, exp: i64, nbf: i64) -> serde_json::Value {
        let now = unix_now();
        serde_json::json!({
            "email": email, "sub": email, "iss": iss, "aud": [aud],
            "exp": now + exp, "nbf": now + nbf, "iat": now,
        })
    }

    fn sign(&self, claims: &serde_json::Value, alg: Algorithm, kid: Option<&str>) -> String {
        let mut hdr = Header::new(alg);
        hdr.kid = kid.map(str::to_string);
        encode(&hdr, claims, &self.key.encoding_key).unwrap()
    }

    fn sign_default(&self, claims: &serde_json::Value) -> String {
        self.sign(claims, Algorithm::RS256, Some(&self.key.kid))
    }

    fn mint(&self, email: &str, iss: &str, aud: &str, exp: i64, nbf: i64) -> String {
        self.sign_default(&self.claims(email, iss, aud, exp, nbf))
    }

    /// A valid token for `email`.
    pub fn valid_token(&self, email: &str) -> String {
        self.mint(email, &self.team_domain, &self.aud, 3600, -1)
    }

    /// A token signed with an unregistered key (unknown kid).
    pub fn unknown_kid_token(&self, email: &str) -> String {
        let throwaway = TestKey::generate("unknown-kid-xyz");
        let claims = self.claims(email, &self.team_domain, &self.aud, 3600, -1);
        let mut hdr = Header::new(Algorithm::RS256);
        hdr.kid = Some("unknown-kid-xyz".to_string());
        encode(&hdr, &claims, &throwaway.encoding_key).unwrap()
    }

    pub fn expired_token(&self, email: &str) -> String {
        self.mint(email, &self.team_domain, &self.aud, -3600, -7200)
    }

    pub fn not_yet_valid_token(&self, email: &str) -> String {
        self.mint(email, &self.team_domain, &self.aud, 7200, 3600)
    }

    pub fn wrong_aud_token(&self, email: &str) -> String {
        self.mint(email, &self.team_domain, "wrong-audience-xyz", 3600, -1)
    }

    pub fn wrong_issuer_token(&self, email: &str) -> String {
        self.mint(
            email,
            "https://evil.cloudflareaccess.com",
            &self.aud,
            3600,
            -1,
        )
    }

    /// The payload bytes modified after signing (signature mismatch).
    pub fn tampered_token(&self, email: &str) -> String {
        let valid = self.valid_token(email);
        let mut parts: Vec<String> = valid.split('.').map(String::from).collect();
        let first = parts[1].chars().next().unwrap();
        let replacement = if first == 'a' { 'b' } else { 'a' };
        parts[1] = format!("{replacement}{}", &parts[1][1..]);
        parts.join(".")
    }

    /// `"alg":"none"` — no signature.
    pub fn alg_none_token(&self, email: &str) -> String {
        let header = URL_SAFE_NO_PAD.encode(format!(
            r#"{{"alg":"none","typ":"JWT","kid":"{}"}}"#,
            self.key.kid
        ));
        let claims = self.claims(email, &self.team_domain, &self.aud, 3600, -1);
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{payload}.")
    }

    /// HS256 keyed with the RSA modulus bytes — the classic algorithm-confusion attack.
    pub fn alg_hs256_pubkey_token(&self, email: &str) -> String {
        let n_bytes = URL_SAFE_NO_PAD.decode(&self.key.n).unwrap();
        let claims = self.claims(email, &self.team_domain, &self.aud, 3600, -1);
        let mut hdr = Header::new(Algorithm::HS256);
        hdr.kid = Some(self.key.kid.clone());
        encode(&hdr, &claims, &EncodingKey::from_secret(&n_bytes)).unwrap()
    }

    /// Mint the negative token the corpus calls `name`, or `None` if this module has no minter
    /// for it.
    pub fn negative_token(&self, name: &str, email: &str) -> Option<String> {
        let ok = |c: &serde_json::Value| self.sign_default(c);
        let good = || self.claims(email, &self.team_domain, &self.aud, 3600, -1);
        let without = |field: &str| {
            let mut c = good();
            c.as_object_mut().unwrap().remove(field);
            c
        };
        Some(match name {
            "garbage" => "not.a.valid.jwt".to_string(),
            "empty" => String::new(),
            "missing_kid" => self.sign(&good(), Algorithm::RS256, None),
            "unknown_kid" => self.unknown_kid_token(email),
            "expired" => self.expired_token(email),
            "not_yet_valid" => self.not_yet_valid_token(email),
            "wrong_audience" => self.wrong_aud_token(email),
            "wrong_issuer" => self.wrong_issuer_token(email),
            "tampered_payload" => self.tampered_token(email),
            "alg_none" => self.alg_none_token(email),
            "alg_hs256_public_key" => self.alg_hs256_pubkey_token(email),
            "alg_rs384" => self.sign(&good(), Algorithm::RS384, Some(&self.key.kid)),
            "no_exp" => ok(&without("exp")),
            "no_email" => ok(&without("email")),
            "no_aud" => ok(&without("aud")),
            "empty_email" => ok(&self.claims("", &self.team_domain, &self.aud, 3600, -1)),
            "two_segments" => {
                let valid = self.valid_token(email);
                valid.rsplit_once('.').unwrap().0.to_string()
            }
            "empty_signature" => {
                let valid = self.valid_token(email);
                format!("{}.", valid.rsplit_once('.').unwrap().0)
            }
            _ => return None,
        })
    }

    /// The signing key as PKCS#8 PEM, for a consumer that must stand up its own JWKS endpoint
    /// (a container test) rather than use this one.
    pub fn signing_key_pem(&self) -> &str {
        &self.key.pem
    }
}

/// Mint a [`VerifiedIdentity`] without going through JWT verification. For tests that exercise
/// tenant resolution or a handler directly; production code has no way to do this.
pub fn identity(email: &str) -> VerifiedIdentity {
    VerifiedIdentity::new(email.to_lowercase(), email.to_string())
}
