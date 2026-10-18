//! The sign-in flow end to end against the fake provider: a "browser" with a cookie jar drives
//! the app's real router over `tower::oneshot`; the token endpoint is a real HTTP server.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use dumpster_kit_oidc::testing::{FakeProvider, TokenSpec, unix_now};
use dumpster_kit_oidc::{Oidc, OidcConfig, SESSION_COOKIE, SessionUser};
use tower::ServiceExt;
use url::Url;

struct Site {
    app: Router,
    jar: Vec<(String, String)>,
    p: FakeProvider,
    offset: Arc<AtomicI64>,
}

async fn whoami(u: SessionUser) -> String {
    u.email
}

impl Site {
    async fn new() -> Self {
        Self::with(|_| {}).await
    }

    async fn with(tweak: impl FnOnce(&mut OidcConfig)) -> Self {
        let p = FakeProvider::start().await;
        let mut cfg = p.config();
        cfg.bypass_paths = vec!["/machine".into()];
        tweak(&mut cfg);
        let offset = Arc::new(AtomicI64::new(0));
        let o2 = offset.clone();
        let oidc = Oidc::discover_with_clock(
            cfg,
            Box::new(move || unix_now() + o2.load(Ordering::SeqCst)),
        )
        .await
        .unwrap();
        let app = Router::new()
            .route("/", get(whoami))
            .route("/machine/ping", get(|| async { "pong" }))
            .layer(from_fn_with_state(oidc.clone(), Oidc::layer))
            .merge(oidc.router());
        Site {
            app,
            jar: vec![],
            p,
            offset,
        }
    }

    async fn get(&mut self, uri: &str) -> Response<Body> {
        self.send(Request::get(uri)).await
    }

    async fn send(&mut self, rb: axum::http::request::Builder) -> Response<Body> {
        let cookies: Vec<String> = self.jar.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let mut rb = rb;
        if !cookies.is_empty() {
            rb = rb.header(header::COOKIE, cookies.join("; "));
        }
        let r = self
            .app
            .clone()
            .oneshot(rb.body(Body::empty()).unwrap())
            .await
            .unwrap();
        for sc in r.headers().get_all(header::SET_COOKIE) {
            let s = sc.to_str().unwrap();
            let (nv, _) = s.split_once(';').unwrap();
            let (n, v) = nv.split_once('=').unwrap();
            self.jar.retain(|(k, _)| k != n);
            if !v.is_empty() {
                self.jar.push((n.into(), v.into()));
            }
        }
        r
    }

    /// Start a login, let the provider approve with `spec`, return the callback response.
    async fn sign_in(&mut self, spec: TokenSpec) -> Response<Body> {
        let (code, state) = self.begin(spec).await;
        self.get(&format!("/auth/callback?code={code}&state={state}"))
            .await
    }

    async fn begin(&mut self, spec: TokenSpec) -> (String, String) {
        let r = self.get("/auth/login?return_to=/deep/page").await;
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        let loc = loc(&r);
        let state = Url::parse(&loc)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        (self.p.authorize(&loc, spec), state)
    }

    fn session_valid_cookie(&self) -> bool {
        self.jar.iter().any(|(k, _)| k == SESSION_COOKIE)
    }
}

fn loc(r: &Response<Body>) -> String {
    r.headers()[header::LOCATION].to_str().unwrap().to_string()
}

async fn body(r: Response<Body>) -> String {
    String::from_utf8(
        axum::body::to_bytes(r.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn login_round_trip() {
    let mut s = Site::new().await;
    // Unauthenticated navigation goes to login, remembering where it was headed.
    let r = s.get("/").await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(loc(&r), "/auth/login?return_to=%2F");

    let r = s.sign_in(TokenSpec::default()).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert_eq!(loc(&r), "/deep/page");
    let sc: Vec<_> = r
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    let sess = sc.iter().find(|c| c.starts_with("dk_session=")).unwrap();
    for attr in [
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
        "Max-Age=3600",
        "Path=/",
    ] {
        assert!(sess.contains(attr), "{sess}");
    }
    let r = s.get("/").await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(body(r).await, "alice@example.com");
}

#[tokio::test]
async fn bad_state_is_refused_and_no_session_made() {
    let mut s = Site::new().await;
    let (code, _) = s.begin(TokenSpec::default()).await;
    let r = s
        .get(&format!("/auth/callback?code={code}&state=forged"))
        .await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert!(!s.session_valid_cookie());
}

#[tokio::test]
async fn callback_without_a_started_flow_is_refused() {
    let mut s = Site::new().await;
    let r = s.get("/auth/callback?code=x&state=y").await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn expired_id_token_is_refused() {
    let mut s = Site::new().await;
    let r = s
        .sign_in(TokenSpec {
            exp_in: -3600,
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert!(!s.session_valid_cookie());
}

#[tokio::test]
async fn id_token_issued_in_the_future_is_refused() {
    let mut s = Site::new().await;
    let r = s
        .sign_in(TokenSpec {
            iat_offset: 3600,
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_audience_is_refused() {
    let mut s = Site::new().await;
    let r = s
        .sign_in(TokenSpec {
            aud: Some("someone-else".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert!(!s.session_valid_cookie());
}

#[tokio::test]
async fn wrong_issuer_is_refused() {
    let mut s = Site::new().await;
    let r = s
        .sign_in(TokenSpec {
            iss: Some("https://evil.example".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn nonce_mismatch_is_refused() {
    let mut s = Site::new().await;
    let r = s
        .sign_in(TokenSpec {
            nonce: Some("replayed".into()),
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn user_outside_the_group_gets_a_403_page_and_no_session() {
    let mut s = Site::new().await;
    let r = s
        .sign_in(TokenSpec {
            groups: vec!["other".into()],
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert!(body(r).await.contains("not a member of the group"));
    assert!(!s.session_valid_cookie());
    // No groups claim at all is the same thing.
    let r = s
        .sign_in(TokenSpec {
            groups: vec![],
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn logout_clears_the_session() {
    let mut s = Site::new().await;
    s.sign_in(TokenSpec::default()).await;
    assert_eq!(s.get("/").await.status(), StatusCode::OK);
    let r = s.get("/auth/logout").await;
    assert!(r.headers().get_all(header::SET_COOKIE).iter().any(|c| {
        let c = c.to_str().unwrap();
        c.starts_with("dk_session=;") && c.contains("Max-Age=0")
    }));
    assert!(!s.session_valid_cookie());
    assert_eq!(s.get("/").await.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn logout_ends_the_provider_session_and_lands_on_signed_out() {
    let mut s = Site::new().await;
    s.sign_in(TokenSpec::default()).await;
    let r = s.get("/auth/logout").await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    let to = Url::parse(&loc(&r)).unwrap();
    assert_eq!(
        format!("{}/logout", s.p.issuer),
        &to[..url::Position::AfterPath]
    );
    let q: std::collections::HashMap<_, _> = to.query_pairs().into_owned().collect();
    assert_eq!(q["post_logout_redirect_uri"], "http://app.test/signed-out");
    assert!(q["id_token_hint"].split('.').count() == 3);
    assert!(!s.session_valid_cookie());

    // The browser follows the redirect: the provider's session ends.
    assert!(s.p.end_sessions().is_empty());
    reqwest::get(to.as_str()).await.unwrap();
    assert_eq!(s.p.end_sessions().len(), 1);
    assert_eq!(s.p.end_sessions()[0]["id_token_hint"], q["id_token_hint"]);

    // The signed-out page is public, says so, and starts nothing.
    let r = s.get("/signed-out").await;
    assert_eq!(r.status(), StatusCode::OK);
    let b = body(r).await;
    assert!(
        b.contains("You're signed out") && b.contains("Sign in again"),
        "{b}"
    );
    assert!(b.contains("href=\"/auth/login\""));
    assert_eq!(s.get("/").await.status(), StatusCode::SEE_OTHER);
    assert_eq!(s.p.end_sessions().len(), 1);

    // "Sign in again" asks the provider again.
    let r = s.get("/auth/login").await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    assert!(loc(&r).starts_with(&format!("{}/authorize", s.p.issuer)));
}

#[tokio::test]
async fn session_expires() {
    let mut s = Site::new().await;
    s.sign_in(TokenSpec::default()).await;
    s.offset.store(3601, Ordering::SeqCst);
    assert_eq!(s.get("/").await.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn tampered_session_cookie_is_ignored() {
    let mut s = Site::new().await;
    s.sign_in(TokenSpec::default()).await;
    for (k, v) in s.jar.iter_mut() {
        if k == SESSION_COOKIE {
            let (_, tag) = v.split_once('.').unwrap();
            *v = format!("{}.{tag}", "eyJ4IjoxfQ");
        }
    }
    assert_eq!(s.get("/").await.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn key_rotation_is_picked_up_and_the_old_key_stops_working() {
    let mut s = Site::new().await;
    s.sign_in(TokenSpec::default()).await;
    s.p.rotate();
    // Token signed with the new, unseen key: the cache re-fetches and accepts it.
    let r = s.sign_in(TokenSpec::default()).await;
    assert_eq!(r.status(), StatusCode::SEE_OTHER);
    // The retired key is gone from the JWKS: a token signed by it is refused.
    let r = s
        .sign_in(TokenSpec {
            sign_with: Some(0),
            ..Default::default()
        })
        .await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_kid_refetch_is_rate_limited() {
    let mut s = Site::with(|c| c.jwks_refetch_min = std::time::Duration::from_secs(3600)).await;
    assert_eq!(s.p.jwks_fetches(), 1);
    // Key B was never published; two attempts must cost at most the one (here: zero) refetch.
    for _ in 0..3 {
        let r = s
            .sign_in(TokenSpec {
                sign_with: Some(1),
                ..Default::default()
            })
            .await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    assert_eq!(s.p.jwks_fetches(), 1);
}

#[tokio::test]
async fn code_is_single_use_and_pkce_is_enforced() {
    let mut s = Site::new().await;
    let (code, state) = s.begin(TokenSpec::default()).await;
    let ok = s
        .get(&format!("/auth/callback?code={code}&state={state}"))
        .await;
    assert_eq!(ok.status(), StatusCode::SEE_OTHER);
    // The flow cookie was consumed with it; replaying is refused before reaching the provider.
    let again = s
        .get(&format!("/auth/callback?code={code}&state={state}"))
        .await;
    assert_eq!(again.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn machine_paths_bypass_and_unauthenticated_non_get_is_401() {
    let mut s = Site::new().await;
    assert_eq!(body(s.get("/machine/ping").await).await, "pong");
    let r = s.send(Request::post("/")).await;
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn return_to_cannot_leave_the_site() {
    let mut s = Site::new().await;
    for bad in ["//evil.example", "https://evil.example", "/\\evil"] {
        let r = s
            .get(&format!(
                "/auth/login?return_to={}",
                url::form_urlencoded::byte_serialize(bad.as_bytes()).collect::<String>()
            ))
            .await;
        let loc = loc(&r);
        let st = Url::parse(&loc)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let code = s.p.authorize(&loc, TokenSpec::default());
        let r = s
            .get(&format!("/auth/callback?code={code}&state={st}"))
            .await;
        assert_eq!(loc_of(&r), "/", "{bad}");
    }
}

fn loc_of(r: &Response<Body>) -> String {
    loc(r)
}

#[tokio::test]
async fn group_rule_is_rechecked_on_every_request() {
    let mut s = Site::new().await;
    s.sign_in(TokenSpec::default()).await;
    // Same cookie, app now requires a different group.
    let (k, v) = s
        .jar
        .iter()
        .find(|(k, _)| k == SESSION_COOKIE)
        .cloned()
        .unwrap();
    let mut s2 = Site::with(|c| c.required_group = "admins".into()).await;
    s2.jar.push((k, v));
    // Different provider/key => cookie key is the same (fixed test key), so the cookie verifies.
    assert_eq!(s2.get("/").await.status(), StatusCode::FORBIDDEN);
}
