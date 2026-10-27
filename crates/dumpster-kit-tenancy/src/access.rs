//! Cloudflare Access JWT validation — the application-layer authentication gate.
//!
//! When configured, every `/api` request must carry a valid Cloudflare Access
//! JWT, extracted from the `Cf-Access-Jwt-Assertion` header or the
//! `CF_Authorization` cookie. The token is verified against the team's published
//! JWKS: RS256 only, with issuer, audience, `exp`, and `nbf` all checked.
//!
//! # Mandatory
//!
//! Access verification is not optional: all three env vars below must be set
//! or the server refuses to start, naming every one that is missing. There is
//! no single-tenant mode, no opt-in switch and no development bypass — no
//! `<PREFIX>_AUTH_DISABLED`, no skip-on-loopback, no debug mode. A missing or
//! invalid token is a 401 with no fallthrough. CI and local development run a
//! real RS256 JWKS server (`crate::testing`, behind the `testing` feature).
//!
//! # Configuration
//!
//! The variable names derive from the application's [`AppPrefix`]:
//!
//! * `<PREFIX>_ACCESS_TEAM_DOMAIN` — full team URL, e.g.
//!   `https://myteam.cloudflareaccess.com`
//! * `<PREFIX>_ACCESS_AUD` — the Access application's audience tag
//! * `<PREFIX>_ACCESS_JWKS_URL` — the JWKS endpoint, **explicit** rather than
//!   derived from the team domain so that test infrastructure can point at a
//!   localhost server without faking the `iss` claim
//!
//! The JWKS is fetched at startup. A failed fetch is a failed
//! startup.
//!
//! # Key cache
//!
//! Keys are cached for 1 hour. An unknown `kid` triggers a re-fetch, but not
//! more than once per 60 seconds, so a stolen or fabricated token cannot be
//! used to hammer the JWKS endpoint.
//!
//! # `VerifiedIdentity`
//!
//! A proof-carrying type whose constructor is private. The only source is
//! [`layer`], which called [`AccessState::verify`]. A handler holding one —
//! obtained via [`current`] — is guaranteed the request was authenticated.
//!
//! The other source is [`VerifiedIdentity::from_session`], for sites that sign in with OIDC.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::app::AppPrefix;
use crate::error::{Result as KitResult, TenancyError};
use crate::http::HttpError;

// ---- public constants -------------------------------------------------------

/// Suffixes of the three variables; the full name is `AppPrefix::var(suffix)`.
pub const TEAM_DOMAIN_SUFFIX: &str = "ACCESS_TEAM_DOMAIN";
pub const AUD_SUFFIX: &str = "ACCESS_AUD";
pub const JWKS_URL_SUFFIX: &str = "ACCESS_JWKS_URL";

// ---- internal constants -----------------------------------------------------

/// The header Cloudflare Access injects on proxied requests.
pub const JWT_HEADER: &str = "cf-access-jwt-assertion";
/// The cookie Cloudflare Access sets for browser sessions.
pub const JWT_COOKIE_NAME: &str = "CF_Authorization";

const CACHE_TTL: Duration = Duration::from_secs(3600);
const REFRESH_RATE_LIMIT: Duration = Duration::from_secs(60);

// ---- configuration ----------------------------------------------------------

/// Startup configuration for the access layer.
pub struct AccessConfig {
    /// Full team URL, e.g. `https://myteam.cloudflareaccess.com`.
    pub team_domain: String,
    /// The Access application's audience tag.
    pub aud: String,
    /// The JWKS endpoint. Explicit rather than derived from `team_domain` so
    /// that test infrastructure can point at localhost without lying about `iss`.
    pub jwks_url: String,
}

impl AccessConfig {
    /// Read the three required env vars. Fails — naming every absent one — if any is missing;
    /// there is no configuration under which the server runs without Access.
    pub fn from_env(app: &AppPrefix) -> KitResult<Self> {
        let get = |suffix: &str| {
            let name = app.var(suffix);
            let value = std::env::var(&name).ok().filter(|v| !v.trim().is_empty());
            (name, value)
        };
        let vars = [
            get(TEAM_DOMAIN_SUFFIX),
            get(AUD_SUFFIX),
            get(JWKS_URL_SUFFIX),
        ];
        let missing: Vec<&str> = vars
            .iter()
            .filter(|(_, v)| v.is_none())
            .map(|(n, _)| n.as_str())
            .collect();
        let [
            (team_name, team_domain),
            (aud_name, aud),
            (jwks_name, jwks_url),
        ] = vars.clone();
        let (Some(team_domain), Some(aud), Some(jwks_url)) = (team_domain, aud, jwks_url) else {
            return Err(TenancyError::Env(format!(
                "refusing to start: Cloudflare Access is not configured ({} not set). \
                 Every request must carry a verified Access JWT and there is no mode \
                 without it; set {team_name}, {aud_name} and {jwks_name}.",
                missing.join(", ")
            )));
        };
        Ok(AccessConfig {
            team_domain,
            aud,
            jwks_url,
        })
    }
}

// ---- JWKS cache -------------------------------------------------------------

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

struct CacheState {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Instant,
    last_refresh_attempt: Option<Instant>,
}

pub(crate) struct JwksCache {
    inner: RwLock<Option<CacheState>>,
    jwks_url: String,
    client: reqwest::Client,
}

impl JwksCache {
    pub(crate) fn new(jwks_url: String, client: reqwest::Client) -> Self {
        JwksCache {
            inner: RwLock::new(None),
            jwks_url,
            client,
        }
    }

    async fn fetch(&self) -> KitResult<HashMap<String, DecodingKey>> {
        let set: JwkSet = async {
            self.client
                .get(&self.jwks_url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await
        }
        .await
        .map_err(|e: reqwest::Error| TenancyError::Env(e.to_string()))?;
        let mut keys = HashMap::new();
        for jwk in set.keys {
            if let Ok(k) = DecodingKey::from_rsa_components(&jwk.n, &jwk.e) {
                keys.insert(jwk.kid, k);
            }
        }
        Ok(keys)
    }

    /// Populate the cache. A failed fetch or an empty key set is fatal.
    pub(crate) async fn prime(&self) -> KitResult<()> {
        let keys = self.fetch().await.map_err(|e| {
            TenancyError::Env(format!("JWKS fetch from {} failed: {e}", self.jwks_url))
        })?;
        if keys.is_empty() {
            return Err(TenancyError::Env(format!(
                "JWKS at {} returned no usable RSA keys",
                self.jwks_url
            )));
        }
        *self.inner.write().await = Some(CacheState {
            keys,
            fetched_at: Instant::now(),
            last_refresh_attempt: None,
        });
        Ok(())
    }

    /// Return the decoding key for `kid`, refreshing when the cache is stale
    /// or the kid is unknown (subject to the rate limit).
    pub(crate) async fn get(&self, kid: &str) -> Result<DecodingKey, HttpError> {
        // Fast path under a read lock.
        {
            let guard = self.inner.read().await;
            if let Some(c) = guard.as_ref()
                && c.fetched_at.elapsed() < CACHE_TTL
                && let Some(k) = c.keys.get(kid)
            {
                return Ok(k.clone());
            }
        }
        // Slow path: stale or unknown kid — acquire the write lock.
        let mut guard = self.inner.write().await;
        // Re-check: another task may have refreshed while we waited.
        if let Some(c) = guard.as_ref()
            && c.fetched_at.elapsed() < CACHE_TTL
        {
            if let Some(k) = c.keys.get(kid) {
                return Ok(k.clone());
            }
            // Unknown kid but within the rate-limit window.
            if c.last_refresh_attempt
                .map(|t| t.elapsed() < REFRESH_RATE_LIMIT)
                .unwrap_or(false)
            {
                return Err(HttpError(
                    StatusCode::UNAUTHORIZED,
                    "unknown signing key".into(),
                ));
            }
        }
        let now = Instant::now();
        match self.fetch().await {
            Ok(keys) => {
                let found = keys.get(kid).cloned();
                *guard = Some(CacheState {
                    keys,
                    fetched_at: now,
                    last_refresh_attempt: Some(now),
                });
                found.ok_or_else(|| {
                    HttpError(StatusCode::UNAUTHORIZED, "unknown signing key".into())
                })
            }
            Err(_) => {
                if let Some(c) = guard.as_mut() {
                    c.last_refresh_attempt = Some(now);
                }
                Err(HttpError(
                    StatusCode::UNAUTHORIZED,
                    "key refresh failed".into(),
                ))
            }
        }
    }
}

// ---- verified identity ------------------------------------------------------

/// Proof that the current request carried a valid Cloudflare Access JWT.
///
/// The constructor is private — the only source is [`layer`], which called
/// [`AccessState::verify`] and succeeded. A handler holding one is guaranteed
/// the request was authenticated by Cloudflare Access.
#[derive(Clone, Debug)]
pub struct VerifiedIdentity {
    /// User's email address, normalised to lowercase.
    email: String,
    /// The JWT `sub` claim — the Cloudflare Access user identity UUID.
    sub: String,
}

impl VerifiedIdentity {
    pub(crate) fn new(email: String, sub: String) -> Self {
        VerifiedIdentity {
            email: email.to_lowercase(),
            sub,
        }
    }

    /// The identity of a user signed in through OIDC (Pocket ID).
    ///
    /// A `SessionUser` is only produced by `dumpster-kit-oidc`'s layer after the ID token was
    /// verified and the groups allow rule passed, so it carries the same proof as an Access JWT.
    /// The `sub` is the OIDC subject; the email is normalised to lowercase like the Access path.
    pub fn from_session(user: &dumpster_kit_oidc::SessionUser) -> Self {
        VerifiedIdentity::new(user.email.clone(), user.sub.clone())
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn sub(&self) -> &str {
        &self.sub
    }
}

tokio::task_local! {
    static CURRENT: VerifiedIdentity;
}

/// The identity verified for the request being served on this task.
pub fn current() -> Result<VerifiedIdentity, HttpError> {
    CURRENT.try_with(VerifiedIdentity::clone).map_err(|_| {
        HttpError::internal(
            "no verified identity for this request — route is outside the access layer",
        )
    })
}

// ---- state and middleware ---------------------------------------------------

#[derive(Deserialize)]
struct Claims {
    email: String,
    sub: String,
}

/// Shared state for the access middleware.
pub struct AccessState {
    aud: String,
    iss: String,
    cache: Arc<JwksCache>,
}

impl AccessState {
    /// Build the access state and prime the JWKS cache.
    ///
    /// Fails if any env var is absent or if the initial JWKS fetch fails —
    /// both are startup failures.
    pub async fn new(cfg: AccessConfig) -> KitResult<Arc<Self>> {
        let iss = cfg.team_domain.trim_end_matches('/').to_string();
        let client = reqwest::Client::new();
        let cache = Arc::new(JwksCache::new(cfg.jwks_url, client));
        cache.prime().await?;
        Ok(Arc::new(AccessState {
            aud: cfg.aud,
            iss,
            cache,
        }))
    }

    pub async fn verify(&self, token: &str) -> Result<VerifiedIdentity, HttpError> {
        let header = jsonwebtoken::decode_header(token)
            .map_err(|_| HttpError(StatusCode::UNAUTHORIZED, "invalid JWT".into()))?;
        let kid = header
            .kid
            .ok_or_else(|| HttpError(StatusCode::UNAUTHORIZED, "JWT missing kid".into()))?;
        let key = self.cache.get(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.aud]);
        validation.set_issuer(&[&self.iss]);
        // jsonwebtoken v9 defaults to validate_nbf = false; enforce it explicitly.
        validation.validate_nbf = true;
        // jsonwebtoken checks `aud` and `iss` only when the token carries them; a token that omits
        // them would otherwise skip both checks. `exp` is required by default, named for clarity.
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| HttpError(StatusCode::UNAUTHORIZED, format!("JWT rejected: {e}")))?;
        // An empty email is a token that proves no one: refuse it here, as a 401, rather than
        // letting it reach the registry and be answered as an unbound identity.
        if data.claims.email.trim().is_empty() {
            return Err(HttpError(
                StatusCode::UNAUTHORIZED,
                "JWT carries no email".into(),
            ));
        }
        Ok(VerifiedIdentity::new(data.claims.email, data.claims.sub))
    }
}

/// Extract the JWT from the `Cf-Access-Jwt-Assertion` header, falling back
/// to the `CF_Authorization` cookie.
pub fn extract_token(req: &axum::http::request::Parts) -> Option<String> {
    if let Some(v) = req.headers.get(JWT_HEADER)
        && let Ok(s) = v.to_str()
    {
        return Some(s.to_string());
    }
    for cookie_hdr in req.headers.get_all(header::COOKIE) {
        if let Ok(s) = cookie_hdr.to_str() {
            for part in s.split(';') {
                let t = part.trim();
                if let Some(v) = t
                    .strip_prefix(JWT_COOKIE_NAME)
                    .and_then(|s| s.strip_prefix('='))
                {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Middleware: verify the Cloudflare Access JWT, then run the rest of the request with the
/// identity in scope. Mount it on `/api` with
/// `axum::middleware::from_fn_with_state(access_state, access::layer)`.
///
/// Completely fail-closed — no token means 401, and there is no path that puts an identity in
/// scope without verifying one.
pub async fn layer(
    State(access): State<Arc<AccessState>>,
    request: Request,
    next: Next,
) -> Result<Response, HttpError> {
    let (parts, body) = request.into_parts();
    let token = extract_token(&parts).ok_or_else(|| {
        HttpError(
            StatusCode::UNAUTHORIZED,
            "missing Cloudflare Access token".into(),
        )
    })?;
    let identity = access.verify(&token).await?;
    let request = Request::from_parts(parts, body);
    Ok(CURRENT.scope(identity, next.run(request)).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{NEGATIVE_TOKENS, TestAccessFixture};
    use axum::http::Request as HttpRequest;

    #[tokio::test]
    async fn valid_token_is_accepted() {
        let fx = TestAccessFixture::new().await;
        let id = fx
            .access
            .verify(&fx.valid_token("Alice@Example.COM"))
            .await
            .unwrap();
        // Email is normalised to lowercase; sub is passed through unchanged.
        assert_eq!(id.email(), "alice@example.com");
        assert_eq!(id.sub(), "Alice@Example.COM");
    }

    /// Every entry of `conformance/jwt/negative.json` is refused with its stated status, and the
    /// fixture can mint exactly the corpus — no entry without a minter, no minter without an entry.
    #[tokio::test]
    async fn every_token_on_the_negative_list_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let corpus = dumpster_kit_conformance::negative_jwts();

        let mut listed: Vec<&str> = corpus.iter().map(|j| j.name.as_str()).collect();
        let mut minted: Vec<&str> = NEGATIVE_TOKENS.to_vec();
        listed.sort();
        minted.sort();
        assert_eq!(
            listed, minted,
            "the corpus and the fixture's minters diverged"
        );

        for case in &corpus {
            let token = fx
                .negative_token(&case.name, "alice@example.com")
                .unwrap_or_else(|| panic!("no minter for {}", case.name));
            let err = fx
                .access
                .verify(&token)
                .await
                .err()
                .unwrap_or_else(|| panic!("{} ({}) was ACCEPTED", case.name, case.describes));
            assert_eq!(err.0.as_u16(), case.status, "{}: {err}", case.name);
        }
    }

    /// The negative list is only a check if the positive control passes under the same fixture.
    #[tokio::test]
    async fn the_negative_tokens_differ_from_the_valid_one_only_in_their_defect() {
        let fx = TestAccessFixture::new().await;
        assert!(
            fx.access
                .verify(&fx.valid_token("a@example.com"))
                .await
                .is_ok()
        );
        for name in NEGATIVE_TOKENS {
            assert_ne!(
                fx.negative_token(name, "a@example.com").unwrap(),
                fx.valid_token("a@example.com"),
                "{name}"
            );
        }
        assert!(
            fx.negative_token("not-a-listed-name", "a@example.com")
                .is_none()
        );
    }

    fn parts(builder: axum::http::request::Builder) -> axum::http::request::Parts {
        builder.body(()).unwrap().into_parts().0
    }

    #[test]
    fn token_is_extracted_from_header_then_cookie() {
        let none = parts(HttpRequest::builder().uri("/api/test"));
        assert!(extract_token(&none).is_none());

        let header = parts(HttpRequest::builder().header(JWT_HEADER, "header-token"));
        assert_eq!(extract_token(&header).as_deref(), Some("header-token"));

        let cookie = parts(
            HttpRequest::builder()
                .header("cookie", format!("{JWT_COOKIE_NAME}=cookie-token; Path=/")),
        );
        assert_eq!(extract_token(&cookie).as_deref(), Some("cookie-token"));

        let both = parts(
            HttpRequest::builder()
                .header(JWT_HEADER, "header-token")
                .header("cookie", format!("{JWT_COOKIE_NAME}=cookie-token")),
        );
        assert_eq!(extract_token(&both).as_deref(), Some("header-token"));
    }

    async fn who() -> Result<String, HttpError> {
        current().map(|i| i.email().to_string())
    }

    fn router(fx: &TestAccessFixture) -> axum::Router {
        axum::Router::new()
            .route("/who", axum::routing::get(who))
            .route_layer(axum::middleware::from_fn_with_state(
                fx.access.clone(),
                layer,
            ))
    }

    #[tokio::test]
    async fn the_layer_is_fail_closed_and_puts_the_verified_identity_in_scope() {
        use tower::ServiceExt;
        let fx = TestAccessFixture::new().await;

        let no_token = router(&fx)
            .oneshot(
                HttpRequest::get("/who")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);

        let bad = router(&fx)
            .oneshot(
                HttpRequest::get("/who")
                    .header(JWT_HEADER, fx.expired_token("a@example.com"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

        let ok = router(&fx)
            .oneshot(
                HttpRequest::get("/who")
                    .header(JWT_HEADER, fx.valid_token("A@Example.com"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let body = axum::body::to_bytes(ok.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"a@example.com");
    }

    #[test]
    fn current_outside_the_layer_is_an_error_not_a_default() {
        assert_eq!(current().unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// There is no configuration without Access: each missing variable is named, the names derive
    /// from the app prefix, and the set of three is the only thing that parses.
    #[test]
    fn config_requires_all_three_variables_named_by_the_prefix() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let app = AppPrefix::new("KITCFG").unwrap();
        let all = [
            (app.var(TEAM_DOMAIN_SUFFIX), "https://t.example"),
            (app.var(AUD_SUFFIX), "aud"),
            (app.var(JWKS_URL_SUFFIX), "http://127.0.0.1:1/jwks"),
        ];
        let set = |skip: Option<&str>| {
            for (k, v) in &all {
                unsafe {
                    if Some(k.as_str()) == skip {
                        std::env::remove_var(k);
                    } else {
                        std::env::set_var(k, v);
                    }
                }
            }
        };
        set(None);
        assert!(AccessConfig::from_env(&app).is_ok());
        for (missing, _) in &all {
            set(Some(missing));
            let err = AccessConfig::from_env(&app)
                .err()
                .expect("started without Access");
            let msg = err.to_string();
            assert!(msg.contains(&format!("({missing} not set)")), "{msg}");
        }
        // An all-blank value is as absent as an unset one.
        unsafe { std::env::set_var(app.var(AUD_SUFFIX), "  ") };
        assert!(AccessConfig::from_env(&app).is_err());
        for (k, _) in &all {
            unsafe { std::env::remove_var(k) };
        }
        let msg = AccessConfig::from_env(&app)
            .err()
            .expect("no Access at all")
            .to_string();
        for (name, _) in &all {
            assert!(msg.contains(name.as_str()), "{msg}");
        }
        assert!(msg.contains("KITCFG_ACCESS_AUD"), "{msg}");
    }

    #[tokio::test]
    async fn a_jwks_that_cannot_be_fetched_is_a_failed_startup() {
        let err = AccessState::new(AccessConfig {
            team_domain: "https://t.example".into(),
            aud: "aud".into(),
            jwks_url: "http://127.0.0.1:1/jwks".into(),
        })
        .await
        .err()
        .expect("started without a JWKS");
        assert!(err.to_string().contains("JWKS fetch"), "{err}");
    }
}
