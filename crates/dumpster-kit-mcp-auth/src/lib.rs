//! The OAuth *resource server* side of a dumpster site's `/mcp`.
//!
//! Pocket ID is the authorization server: it registers the client, signs the user in, asks for
//! consent, issues the access token and revokes grants. This crate only
//!
//! * serves RFC 9728 protected-resource metadata ([`McpAuth::metadata_router`]) naming Pocket
//!   ID's issuer as the authorization server and the site's `/mcp` URL as the resource;
//! * validates the bearer access token on `/mcp` ([`McpAuth::layer`]): RS256 signature from
//!   Pocket ID's JWKS (cached, re-fetched on an unknown `kid`), `iss`, `aud` = this site's
//!   `/mcp` resource, `exp`;
//! * gates on the site's required group and resolves the user to a tenant through the same
//!   registry the browser session uses (`Tenants::serve_session`), so a valid signature alone
//!   never admits anyone;
//! * answers a refusal with `401` + `WWW-Authenticate: Bearer resource_metadata="..."` (or `403`
//!   for a valid token that may not connect), which is how a client finds Pocket ID.
//!
//! # The group gate
//!
//! Pocket ID APIs cannot restrict by user group. If the token carries a `groups` claim it must
//! contain the required group. If it carries none, the registry decides: the token's `email`
//! must be bound to an active tenant, else 403.
//!
//! # Boundary
//!
//! A token is honoured only on the `/mcp` path; [`McpAuth::layer`] refuses a bearer anywhere
//! else, and the site's `/api` and UI sit behind the cookie-session layer, which never reads an
//! `Authorization` header.
//!
//! Known risk: pocket-id issue #1658 — `prompt=consent` together with `resource` returned
//! `invalid_request`, which can stop a client (claude.ai) at the authorization step. That is
//! upstream of this crate; nothing here works around it.
//!
//! Metrics (no tenant, email or subject in any attribute):
//! `dumpster_mcp_auth_validations_total{outcome}` and
//! `dumpster_mcp_auth_refusals_total{reason}`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{OriginalUri, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use dumpster_kit_oidc::SessionUser;
use dumpster_kit_oidc::jwks::JwksCache;
use dumpster_kit_telemetry::{Counter, Telemetry};
use dumpster_kit_tenancy::Tenants;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, Validation};
use serde::Deserialize;
use serde_json::json;
use url::Url;

pub const VALIDATIONS_METRIC: &str = "dumpster_mcp_auth_validations_total";
pub const REFUSALS_METRIC: &str = "dumpster_mcp_auth_refusals_total";
pub const METADATA_PATH: &str = "/.well-known/oauth-protected-resource";

#[derive(Debug, thiserror::Error)]
pub enum McpAuthError {
    #[error("config: {0}")]
    Config(String),
    #[error("provider: {0}")]
    Provider(String),
}

#[derive(Clone, Debug)]
pub struct McpAuthConfig {
    /// Pocket ID's issuer URL; the authorization server named in the metadata.
    pub issuer: String,
    /// This site's `/mcp` URL, e.g. `https://dumpstar.example/mcp`: the token's required `aud`.
    pub resource: String,
    pub required_group: String,
    pub clock_skew: Duration,
    pub jwks_refetch_min: Duration,
}

/// Who is calling `/mcp`; present in request extensions behind [`McpAuth::layer`]. The tenant is
/// in scope as `dumpster_kit_tenancy::tenant::current()`.
#[derive(Clone, Debug)]
pub struct McpCaller {
    /// The OAuth client's name or id as the token states it, for the audit line.
    pub client: String,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
    #[serde(default)]
    userinfo_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    groups: Option<Vec<String>>,
    exp: i64,
    #[serde(default)]
    nbf: Option<i64>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    azp: Option<String>,
}

pub struct McpAuth {
    cfg: McpAuthConfig,
    resource_path: String,
    metadata_url: String,
    jwks: JwksCache,
    http: reqwest::Client,
    userinfo: Option<String>,
    tenants: Arc<Tenants>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
    validations: Counter,
    refusals: Counter,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Why a request was refused. The reason is the metric attribute.
struct Refusal {
    status: StatusCode,
    reason: &'static str,
    /// A token was presented (so the challenge says `invalid_token`).
    presented: bool,
    /// Said to the caller beside the reason, when there is something it can do about it.
    detail: Option<String>,
}

impl McpAuth {
    /// Fetch Pocket ID's discovery and prime the JWKS. A failure is a failed startup.
    pub async fn discover(
        cfg: McpAuthConfig,
        tenants: Arc<Tenants>,
        telemetry: &Telemetry,
    ) -> Result<Arc<Self>, McpAuthError> {
        Self::discover_with_clock(cfg, tenants, telemetry, Box::new(unix_now)).await
    }

    pub async fn discover_with_clock(
        mut cfg: McpAuthConfig,
        tenants: Arc<Tenants>,
        telemetry: &Telemetry,
        clock: Box<dyn Fn() -> i64 + Send + Sync>,
    ) -> Result<Arc<Self>, McpAuthError> {
        cfg.issuer = cfg.issuer.trim_end_matches('/').to_string();
        let resource = Url::parse(&cfg.resource)
            .map_err(|e| McpAuthError::Config(format!("resource: {e}")))?;
        let resource_path = resource.path().trim_end_matches('/').to_string();
        if resource_path.is_empty() {
            return Err(McpAuthError::Config(
                "resource must be the site's /mcp URL, not its origin".into(),
            ));
        }
        let mut meta = resource.clone();
        meta.set_path(&format!("{METADATA_PATH}{resource_path}"));
        meta.set_query(None);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| McpAuthError::Provider(e.to_string()))?;
        let url = format!("{}/.well-known/openid-configuration", cfg.issuer);
        let disc: Discovery = http
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| McpAuthError::Provider(format!("discovery {url}: {e}")))?
            .json()
            .await
            .map_err(|e| McpAuthError::Provider(format!("discovery body: {e}")))?;
        if disc.issuer.trim_end_matches('/') != cfg.issuer {
            return Err(McpAuthError::Provider(format!(
                "discovery issuer {} does not match configured {}",
                disc.issuer, cfg.issuer
            )));
        }
        let jwks = JwksCache::new(disc.jwks_uri, http.clone(), cfg.jwks_refetch_min);
        jwks.prime()
            .await
            .map_err(|e| McpAuthError::Provider(e.to_string()))?;
        Ok(Arc::new(McpAuth {
            cfg,
            resource_path,
            metadata_url: meta.to_string(),
            jwks,
            http,
            userinfo: disc.userinfo_endpoint,
            tenants,
            clock,
            validations: telemetry.counter(
                VALIDATIONS_METRIC,
                "Bearer access tokens presented on /mcp, by outcome",
            ),
            refusals: telemetry.counter(REFUSALS_METRIC, "Requests to /mcp refused, by reason"),
        }))
    }

    /// The RFC 9728 metadata, at the root path and at the path-inserted form for `/mcp`.
    pub fn metadata_router(self: &Arc<Self>) -> Router {
        let doc = json!({
            "resource": self.cfg.resource,
            "authorization_servers": [self.cfg.issuer],
            "bearer_methods_supported": ["header"],
        });
        let handler = move || {
            let doc = doc.clone();
            async move { axum::Json(doc) }
        };
        Router::new()
            .route(METADATA_PATH, get(handler.clone()))
            .route(
                &format!("{METADATA_PATH}{}", self.resource_path),
                get(handler),
            )
    }

    fn on_mcp(&self, path: &str) -> bool {
        path == self.resource_path
            || path
                .strip_prefix(&self.resource_path)
                .is_some_and(|r| r.starts_with('/'))
    }

    async fn check(
        &self,
        path: &str,
        authorization: Option<&str>,
    ) -> Result<(SessionUser, String, String), Refusal> {
        let refuse = |status, reason, presented| Refusal {
            status,
            reason,
            presented,
            detail: None,
        };
        let presented = authorization.is_some();
        if !self.on_mcp(path) {
            return Err(refuse(StatusCode::UNAUTHORIZED, "wrong_path", presented));
        }
        let token = authorization
            .and_then(|v| {
                let (scheme, rest) = v.split_once(' ')?;
                scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
            })
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                refuse(
                    StatusCode::UNAUTHORIZED,
                    if presented {
                        "malformed"
                    } else {
                        "missing_token"
                    },
                    false,
                )
            })?;
        let bad = |reason| refuse(StatusCode::UNAUTHORIZED, reason, true);
        let header = jsonwebtoken::decode_header(token).map_err(|_| bad("malformed"))?;
        if header.alg != Algorithm::RS256 {
            return Err(bad("bad_algorithm"));
        }
        let kid = header.kid.ok_or_else(|| bad("unknown_key"))?;
        let key = self
            .jwks
            .get(&kid)
            .await
            .ok_or_else(|| bad("unknown_key"))?;
        let mut v = Validation::new(Algorithm::RS256);
        v.set_audience(&[&self.cfg.resource]);
        v.set_issuer(&[&self.cfg.issuer]);
        v.set_required_spec_claims(&["exp", "aud", "iss", "sub"]);
        v.leeway = 0;
        // jsonwebtoken reads the system clock; time is checked against ours below.
        v.validate_exp = false;
        v.validate_nbf = false;
        let claims = jsonwebtoken::decode::<Claims>(token, &key, &v)
            .map_err(|e| {
                bad(match e.kind() {
                    ErrorKind::InvalidAudience => "bad_audience",
                    ErrorKind::InvalidIssuer => "bad_issuer",
                    ErrorKind::InvalidSignature => "bad_signature",
                    _ => "malformed",
                })
            })?
            .claims;
        let now = (self.clock)();
        let skew = self.cfg.clock_skew.as_secs() as i64;
        if claims.exp + skew <= now {
            return Err(bad("expired"));
        }
        if claims.nbf.is_some_and(|n| n > now + skew) {
            return Err(bad("not_yet_valid"));
        }
        if claims.sub.is_empty() {
            return Err(bad("no_subject"));
        }
        let forbid = |reason| refuse(StatusCode::FORBIDDEN, reason, true);
        if let Some(groups) = &claims.groups
            && !groups.contains(&self.cfg.required_group)
        {
            return Err(forbid("not_in_group"));
        }
        // Pocket ID API access tokens carry only `sub`: an empty email is resolved by `sub`, which
        // the signature just verified, against what the site learned at browser sign-in.
        let email = claims.email.filter(|e| !e.is_empty()).unwrap_or_default();
        let client = claims
            .client_id
            .or(claims.azp)
            .unwrap_or_else(|| "unknown".into());
        let user = SessionUser {
            sub: claims.sub,
            email,
            name: claims.name.unwrap_or_default(),
            groups: claims.groups.unwrap_or_default(),
            exp: claims.exp,
        };
        Ok((user, client, token.to_string()))
    }

    /// Middleware for the `/mcp` router, with
    /// `axum::middleware::from_fn_with_state(auth, McpAuth::layer)`.
    pub async fn layer(State(a): State<Arc<McpAuth>>, mut req: Request, next: Next) -> Response {
        let path = req
            .extensions()
            .get::<OriginalUri>()
            .map(|o| o.0.path().to_string())
            .unwrap_or_else(|| req.uri().path().to_string());
        let authorization = req
            .headers()
            .get(header::AUTHORIZATION)
            .map(|v| v.to_str().unwrap_or("").to_string());
        let (user, client, token) = match a.check(&path, authorization.as_deref()).await {
            Ok(ok) => ok,
            Err(r) => return a.refuse(r),
        };
        req.extensions_mut().insert(McpCaller {
            client: client.clone(),
        });
        let served = if user.email.is_empty() {
            // A subject not yet learned is asked of Pocket ID itself, with the token it signed,
            // so an agent-only user never has to open the website.
            if a.tenants.resolve_subject(&user.sub, &a.cfg.issuer).is_err()
                && let Some(known) = a.userinfo_user(&user, &token).await
            {
                a.tenants.serve_session(&known, next.run(req)).await
            } else {
                // Userinfo named no email (an empty-scope token): follow the sub's learned
                // address onto the tenant now active for it.
                if a.tenants.resolve_subject(&user.sub, &a.cfg.issuer).is_err() {
                    a.tenants.rebind_subject(&user.sub, &a.cfg.issuer);
                }
                a.tenants
                    .serve_subject(&user.sub, &a.cfg.issuer, next.run(req))
                    .await
            }
        } else {
            a.tenants.serve_session(&user, next.run(req)).await
        };
        match served {
            Ok(resp) => {
                a.validations.add(1, &[("outcome", "accepted")]);
                eprintln!("mcp-auth: client={client} accepted");
                resp
            }
            Err(e) => a.refuse(Refusal {
                status: StatusCode::FORBIDDEN,
                reason: "no_tenant",
                presented: true,
                detail: Some(e.1),
            }),
        }
    }

    /// Pocket ID's own answer for this token's subject: the verified identity the registry can
    /// bind. Nothing is trusted unless userinfo names the same `sub` and an email, and, if it
    /// states groups, the required group.
    async fn userinfo_user(&self, user: &SessionUser, token: &str) -> Option<SessionUser> {
        #[derive(Deserialize)]
        struct Info {
            sub: String,
            #[serde(default)]
            email: Option<String>,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            groups: Option<Vec<String>>,
        }
        let url = self.userinfo.as_ref()?;
        let info: Info = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        let email = info.email.filter(|e| !e.is_empty())?;
        if info.sub != user.sub {
            return None;
        }
        let groups = info.groups.unwrap_or_default();
        if !groups.is_empty() && !groups.contains(&self.cfg.required_group) {
            return None;
        }
        Some(SessionUser {
            sub: info.sub,
            email,
            name: info.name.unwrap_or_default(),
            groups,
            exp: user.exp,
        })
    }

    fn refuse(&self, r: Refusal) -> Response {
        self.validations.add(1, &[("outcome", "refused")]);
        self.refusals.add(1, &[("reason", r.reason)]);
        match &r.detail {
            Some(d) => eprintln!("mcp-auth: refused reason={} detail={d}", r.reason),
            None => eprintln!("mcp-auth: refused reason={}", r.reason),
        }
        let challenge = if r.status == StatusCode::FORBIDDEN {
            format!(
                "Bearer error=\"insufficient_scope\", resource_metadata=\"{}\"",
                self.metadata_url
            )
        } else if r.presented {
            format!(
                "Bearer error=\"invalid_token\", resource_metadata=\"{}\"",
                self.metadata_url
            )
        } else {
            format!("Bearer resource_metadata=\"{}\"", self.metadata_url)
        };
        let mut resp = (
            r.status,
            axum::Json(json!({"error": r.reason, "detail": r.detail})),
        )
            .into_response();
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_str(&challenge).expect("ascii challenge"),
        );
        resp
    }
}
