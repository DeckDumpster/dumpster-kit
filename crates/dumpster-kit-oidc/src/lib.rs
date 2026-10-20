//! OIDC sign-in for the dumpster sites, against Pocket ID.
//!
//! The app signs users in itself — authorization-code flow with PKCE, `state` and `nonce` —
//! instead of sitting behind Cloudflare Access's login page.
//!
//! * [`Oidc::router`] mounts `/auth/login`, `/auth/callback` and `/auth/logout`.
//! * `/signed-out` is a public page ("You're signed out", a "Sign in again" button); it never
//!   starts a sign-in. `/auth/logout` clears the app session and then sends the browser to the
//!   provider's `end_session_endpoint` (when discovery has one) with `id_token_hint` and
//!   `post_logout_redirect_uri = <site>/signed-out`, so the provider's own session ends too —
//!   register that URI on the client.
//! * [`Oidc::layer`] (use with `axum::middleware::from_fn_with_state`) requires a session on
//!   everything else, except the configured bypass paths (machine endpoints that carry their own
//!   credential) and `/auth/*`. A handler then takes a [`SessionUser`].
//! * The ID token is verified on callback: RS256 against the provider's JWKS (cached, re-fetched
//!   on key rotation), `iss`, `aud`, `exp`, `iat`, `nonce`. The `groups` claim must contain the
//!   configured group; a signed-in user outside it gets a 403 page and no session.
//! * The session is a signed cookie (`HttpOnly; Secure; SameSite=Lax`) with an expiry; the group
//!   rule is applied again on every request, so changing `REQUIRED_GROUP` takes effect at once.
//!
//! See [`config`] for the environment. With the `testing` feature, [`testing::FakeProvider`] is
//! an in-process provider for a consumer's own tests.

pub mod config;
pub mod error;
/// The provider's signing keys, for a sibling crate validating tokens the same provider issued.
pub mod jwks;
mod seal;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use jsonwebtoken::{Algorithm, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

pub use config::OidcConfig;
pub use error::{AuthError, OidcError};

use jwks::JwksCache;
use seal::Sealer;

pub const SESSION_COOKIE: &str = "dk_session";
pub const FLOW_COOKIE: &str = "dk_oidc_flow";
pub const ID_TOKEN_COOKIE: &str = "dk_id_token";
pub const SIGNED_OUT_PATH: &str = "/signed-out";
const FLOW_TTL_SECS: i64 = 600;
const SCOPES: &str = "openid email profile groups";

/// The signed-in user. Present in request extensions behind [`Oidc::layer`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionUser {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub groups: Vec<String>,
    pub exp: i64,
}

impl<S: Send + Sync> FromRequestParts<S> for SessionUser {
    type Rejection = StatusCode;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, StatusCode> {
        parts
            .extensions
            .get::<SessionUser>()
            .cloned()
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

#[derive(Serialize, Deserialize)]
struct Flow {
    state: String,
    nonce: String,
    verifier: String,
    return_to: String,
    exp: i64,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    end_session_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    preferred_username: String,
    #[serde(default)]
    groups: Vec<String>,
    nonce: Option<String>,
    iat: Option<i64>,
    exp: i64,
}

pub struct Oidc {
    cfg: OidcConfig,
    disc: Discovery,
    http: reqwest::Client,
    jwks: JwksCache,
    sealer: Sealer,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn random_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut b);
    B64.encode(b)
}

impl Oidc {
    /// Fetch discovery and prime the JWKS. A failure is a failed startup.
    pub async fn discover(cfg: OidcConfig) -> Result<Arc<Self>, OidcError> {
        Self::discover_with_clock(cfg, Box::new(unix_now)).await
    }

    /// As [`discover`](Self::discover), with the clock the session and token expiry read.
    pub async fn discover_with_clock(
        cfg: OidcConfig,
        clock: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Arc<Self>, OidcError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| OidcError::Provider(e.to_string()))?;
        let url = format!("{}/.well-known/openid-configuration", cfg.issuer);
        let disc: Discovery = http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| OidcError::Provider(format!("discovery {url}: {e}")))?
            .json()
            .await
            .map_err(|e| OidcError::Provider(format!("discovery body: {e}")))?;
        if disc.issuer.trim_end_matches('/') != cfg.issuer {
            return Err(OidcError::Provider(format!(
                "discovery issuer {} does not match configured {}",
                disc.issuer, cfg.issuer
            )));
        }
        let jwks = JwksCache::new(disc.jwks_uri.clone(), http.clone(), cfg.jwks_refetch_min);
        jwks.prime().await?;
        let sealer = Sealer::new(cfg.cookie_key.clone());
        Ok(Arc::new(Oidc {
            cfg,
            disc,
            http,
            jwks,
            sealer,
            clock,
        }))
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    /// `/auth/login`, `/auth/callback`, `/auth/logout`.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/auth/login", get(login))
            .route("/auth/callback", get(callback))
            .route("/auth/logout", get(logout).post(logout))
            .route(SIGNED_OUT_PATH, get(signed_out))
            .with_state(self.clone())
    }

    /// Require a session. Browsers navigating (GET) are sent to `/auth/login`; everything else
    /// gets 401. A signed-in user outside the required group gets the 403 page.
    pub async fn layer(State(o): State<Arc<Oidc>>, mut req: Request, next: Next) -> Response {
        let path = req.uri().path().to_string();
        if path.starts_with("/auth/") || path == SIGNED_OUT_PATH || o.bypassed(&path) {
            return next.run(req).await;
        }
        match o.session(req.headers()) {
            Some(u) if u.groups.contains(&o.cfg.required_group) => {
                req.extensions_mut().insert(u);
                next.run(req).await
            }
            Some(_) => forbidden_page(),
            None if req.method() == axum::http::Method::GET => {
                let rt = req
                    .uri()
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/");
                let mut u = Url::parse("http://x/auth/login").unwrap();
                u.query_pairs_mut().append_pair("return_to", rt);
                Redirect::to(&format!("/auth/login?{}", u.query().unwrap())).into_response()
            }
            None => StatusCode::UNAUTHORIZED.into_response(),
        }
    }

    /// Where logout sends the browser: the provider's end-session endpoint, or straight to the
    /// signed-out page when the provider advertises none.
    fn logout_target(&self, id_token_hint: Option<&str>) -> String {
        let back = Url::parse(&self.cfg.redirect_url)
            .map(|mut u| {
                u.set_path(SIGNED_OUT_PATH);
                u.set_query(None);
                u.to_string()
            })
            .unwrap_or_else(|_| SIGNED_OUT_PATH.to_string());
        let Some(Ok(mut url)) = self.disc.end_session_endpoint.as_deref().map(Url::parse) else {
            return SIGNED_OUT_PATH.to_string();
        };
        {
            let mut q = url.query_pairs_mut();
            if let Some(h) = id_token_hint {
                q.append_pair("id_token_hint", h);
            }
            q.append_pair("post_logout_redirect_uri", &back)
                .append_pair("client_id", &self.cfg.client_id);
        }
        url.to_string()
    }

    fn bypassed(&self, path: &str) -> bool {
        self.cfg.bypass_paths.iter().any(|p| {
            let p = p.trim_end_matches('/');
            path == p || path.starts_with(&format!("{p}/"))
        })
    }

    fn session(&self, headers: &HeaderMap) -> Option<SessionUser> {
        let raw = cookie(headers, SESSION_COOKIE)?;
        let u: SessionUser =
            serde_json::from_slice(&self.sealer.open(SESSION_COOKIE, &raw)?).ok()?;
        (u.exp > self.now()).then_some(u)
    }

    async fn verify_id_token(&self, token: &str, nonce: &str) -> Result<Claims, AuthError> {
        let bad = |m: &'static str| AuthError(StatusCode::UNAUTHORIZED, m);
        let header = jsonwebtoken::decode_header(token).map_err(|_| bad("invalid id_token"))?;
        let kid = header.kid.ok_or(bad("id_token has no kid"))?;
        let key = self
            .jwks
            .get(&kid)
            .await
            .ok_or(bad("id_token signed by unknown key"))?;
        let mut v = Validation::new(Algorithm::RS256);
        v.set_audience(&[&self.cfg.client_id]);
        v.set_issuer(&[&self.cfg.issuer]);
        v.set_required_spec_claims(&["exp", "aud", "iss", "iat"]);
        v.leeway = 0;
        v.validate_exp = false;
        let data = jsonwebtoken::decode::<Claims>(token, &key, &v)
            .map_err(|_| bad("id_token rejected"))?;
        let c = data.claims;
        // jsonwebtoken reads the system clock; check time against ours so one clock rules.
        let skew = self.cfg.clock_skew.as_secs() as i64;
        if c.exp + skew <= self.now() || c.iat.is_none_or(|i| i > self.now() + skew) {
            return Err(bad("id_token outside its validity"));
        }
        if c.nonce.as_deref() != Some(nonce) {
            return Err(bad("nonce mismatch"));
        }
        if c.sub.is_empty() {
            return Err(bad("id_token has no subject"));
        }
        Ok(c)
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|s| s.split(';'))
        .find_map(|p| {
            p.trim()
                .strip_prefix(name)?
                .strip_prefix('=')
                .map(str::to_string)
        })
}

fn set_cookie(name: &str, value: &str, path: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{name}={value}; Path={path}; HttpOnly; Secure; SameSite=Lax; Max-Age={max_age}"
    ))
    .expect("cookie values are base64url")
}

/// Only a path on this site: a leading `/`, no `//`, no backslash, no control characters.
fn safe_return_to(s: &str) -> &str {
    let ok = s.starts_with('/')
        && !s.starts_with("//")
        && !s.contains('\\')
        && !s.chars().any(|c| c.is_control());
    if ok { s } else { "/" }
}

fn forbidden_page() -> Response {
    (
        StatusCode::FORBIDDEN,
        Html(
            "<!doctype html><title>Access denied</title><h1>Access denied</h1>\
             <p>You are signed in, but your account is not a member of the group \
             this site requires. Ask the owner to add you, then \
             <a href=\"/auth/logout\">sign out</a> and back in.</p>",
        ),
    )
        .into_response()
}

async fn login(State(o): State<Arc<Oidc>>, Query(q): Query<HashMap<String, String>>) -> Response {
    let flow = Flow {
        state: random_b64(24),
        nonce: random_b64(24),
        verifier: random_b64(48),
        return_to: safe_return_to(q.get("return_to").map(String::as_str).unwrap_or("/"))
            .to_string(),
        exp: o.now() + FLOW_TTL_SECS,
    };
    let challenge = B64.encode(Sha256::digest(flow.verifier.as_bytes()));
    let Ok(mut url) = Url::parse(&o.disc.authorization_endpoint) else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &o.cfg.client_id)
        .append_pair("redirect_uri", &o.cfg.redirect_url)
        .append_pair("scope", SCOPES)
        .append_pair("state", &flow.state)
        .append_pair("nonce", &flow.nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let sealed = o
        .sealer
        .seal(FLOW_COOKIE, &serde_json::to_vec(&flow).unwrap());
    let mut resp = Redirect::to(url.as_str()).into_response();
    resp.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(FLOW_COOKIE, &sealed, "/auth", FLOW_TTL_SECS),
    );
    resp
}

async fn callback(
    State(o): State<Arc<Oidc>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AuthError> {
    let flow: Flow = cookie(&headers, FLOW_COOKIE)
        .and_then(|c| o.sealer.open(FLOW_COOKIE, &c))
        .and_then(|b| serde_json::from_slice(&b).ok())
        .filter(|f: &Flow| f.exp > o.now())
        .ok_or(AuthError(
            StatusCode::BAD_REQUEST,
            "sign-in expired or not started; try again",
        ))?;
    if q.get("state").map(String::as_str) != Some(flow.state.as_str()) {
        return Err(AuthError(StatusCode::BAD_REQUEST, "state mismatch"));
    }
    if q.contains_key("error") {
        return Err(AuthError(
            StatusCode::UNAUTHORIZED,
            "the provider refused the sign-in",
        ));
    }
    let code = q
        .get("code")
        .ok_or(AuthError(StatusCode::BAD_REQUEST, "no authorization code"))?;
    let resp = o
        .http
        .post(&o.disc.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &o.cfg.redirect_url),
            ("client_id", &o.cfg.client_id),
            ("client_secret", &o.cfg.client_secret),
            ("code_verifier", &flow.verifier),
        ])
        .send()
        .await
        .map_err(|_| AuthError(StatusCode::BAD_GATEWAY, "provider unreachable"))?;
    if !resp.status().is_success() {
        return Err(AuthError(
            StatusCode::UNAUTHORIZED,
            "the provider refused the code",
        ));
    }
    let tok: TokenResponse = resp
        .json()
        .await
        .map_err(|_| AuthError(StatusCode::BAD_GATEWAY, "malformed token response"))?;
    let c = o.verify_id_token(&tok.id_token, &flow.nonce).await?;

    let clear_flow = set_cookie(FLOW_COOKIE, "", "/auth", 0);
    if !c.groups.contains(&o.cfg.required_group) {
        let mut r = forbidden_page();
        r.headers_mut().append(header::SET_COOKIE, clear_flow);
        return Ok(r);
    }
    let ttl = o.cfg.session_ttl.as_secs() as i64;
    let user = SessionUser {
        sub: c.sub,
        email: c.email.to_lowercase(),
        name: if c.name.is_empty() {
            c.preferred_username
        } else {
            c.name
        },
        groups: c.groups,
        exp: o.now() + ttl,
    };
    let sealed = o
        .sealer
        .seal(SESSION_COOKIE, &serde_json::to_vec(&user).unwrap());
    let mut r = Redirect::to(&flow.return_to).into_response();
    r.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(SESSION_COOKIE, &sealed, "/", ttl),
    );
    // Kept only to prove to the provider, at sign-out, which session to end.
    let hint = o.sealer.seal(ID_TOKEN_COOKIE, tok.id_token.as_bytes());
    r.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(ID_TOKEN_COOKIE, &hint, "/auth", ttl),
    );
    r.headers_mut().append(header::SET_COOKIE, clear_flow);
    Ok(r)
}

async fn logout(State(o): State<Arc<Oidc>>, headers: HeaderMap) -> Response {
    let hint = cookie(&headers, ID_TOKEN_COOKIE)
        .and_then(|c| o.sealer.open(ID_TOKEN_COOKIE, &c))
        .and_then(|b| String::from_utf8(b).ok());
    let mut r = Redirect::to(&o.logout_target(hint.as_deref())).into_response();
    r.headers_mut()
        .append(header::SET_COOKIE, set_cookie(SESSION_COOKIE, "", "/", 0));
    r.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(ID_TOKEN_COOKIE, "", "/auth", 0),
    );
    r
}

async fn signed_out() -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(
            "<!doctype html><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
             <title>Signed out</title><h1>You're signed out</h1>\
             <p><a href=\"/auth/login\"><button type=\"button\">Sign in again</button></a></p>",
        ),
    )
        .into_response()
}
