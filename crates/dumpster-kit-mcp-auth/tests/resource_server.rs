//! The resource server against a fake Pocket ID (JWKS + discovery) and a real tenant registry.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::middleware::from_fn_with_state;
use axum::routing::get;
use dumpster_kit_mcp_auth::{McpAuth, McpAuthConfig, McpCaller};
use dumpster_kit_oidc::testing::{FakeProvider, GROUP, unix_now};
use dumpster_kit_telemetry::Telemetry;
use dumpster_kit_tenancy::connection::{self, TenantDbSpec};
use dumpster_kit_tenancy::schema_version::SchemaSpec;
use dumpster_kit_tenancy::{AppPrefix, Tenants, registry, tenant};
use serde_json::{Value, json};
use tower::ServiceExt;

const RESOURCE: &str = "https://site.test/mcp";
const SHARED: SchemaSpec = SchemaSpec {
    label: "shared catalog",
    version: 1,
};
const SPEC: TenantDbSpec = TenantDbSpec::new(
    "CREATE TABLE IF NOT EXISTS binders (id INTEGER PRIMARY KEY, name TEXT);",
    SchemaSpec {
        label: "collection",
        version: 1,
    },
    SHARED,
);

fn tenants(dir: &Path, handles: &[&str]) -> Arc<Tenants> {
    let shared = dir.join("shared.sqlite");
    rusqlite::Connection::open(&shared)
        .unwrap()
        .execute_batch("CREATE TABLE cards (id INTEGER PRIMARY KEY); PRAGMA user_version = 1;")
        .unwrap();
    let tdir = dir.join("tenants");
    std::fs::create_dir_all(&tdir).unwrap();
    let reg_db = dir.join("registry.sqlite");
    let reg = connection::open_registry(&reg_db).unwrap();
    for h in handles {
        let u = registry::insert(&reg, h).unwrap();
        connection::open_tenant(
            &dumpster_kit_tenancy::tenant_db_file(&tdir, &u.database_id).unwrap(),
            &SPEC,
        )
        .unwrap();
        registry::identity_add(&reg, &u.database_id, &format!("{h}@test.local"), None, None)
            .unwrap();
    }
    Arc::new(
        Tenants::new(
            AppPrefix::new("KITTEST").unwrap(),
            tdir,
            shared,
            &reg_db,
            SPEC,
        )
        .unwrap(),
    )
}

struct Fixture {
    idp: FakeProvider,
    app: Router,
    tenants: Arc<Tenants>,
    clock: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let tenants = tenants(dir.path(), &["alice", "bob"]);
    let idp = FakeProvider::start().await;
    let clock = Arc::new(AtomicI64::new(unix_now()));
    let c = clock.clone();
    let auth = McpAuth::discover_with_clock(
        McpAuthConfig {
            issuer: idp.issuer.clone(),
            resource: RESOURCE.into(),
            required_group: GROUP.into(),
            clock_skew: std::time::Duration::from_secs(5),
            jwks_refetch_min: std::time::Duration::ZERO,
        },
        tenants.clone(),
        &Telemetry::disabled(),
        Box::new(move || c.load(Ordering::SeqCst)),
    )
    .await
    .unwrap();
    // /mcp reports which tenant and client it served; /api is the cookie-session surface, which
    // never reads a bearer.
    let mcp = Router::new()
        .route(
            "/mcp",
            get(
                |axum::Extension(c): axum::Extension<McpCaller>| async move {
                    format!("{}|{}", tenant::current().unwrap(), c.client)
                },
            ),
        )
        .layer(from_fn_with_state(auth.clone(), McpAuth::layer));
    let api = Router::new()
        .route("/api/cards", get(|| async { "secret" }))
        .layer(axum::middleware::from_fn(session_only));
    let app = Router::new()
        .merge(auth.metadata_router())
        .merge(mcp)
        .merge(api);
    Fixture {
        idp,
        app,
        tenants,
        clock,
        _dir: dir,
    }
}

/// Stands in for `Oidc::layer`: no session cookie, no entry; an `Authorization` header is never read.
async fn session_only(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if req.headers().get(header::COOKIE).is_none() {
        return axum::response::IntoResponse::into_response(StatusCode::UNAUTHORIZED);
    }
    next.run(req).await
}

impl Fixture {
    fn claims(&self, email: &str) -> Value {
        json!({
            "iss": self.idp.issuer, "aud": RESOURCE, "sub": format!("sub-{email}"),
            "email": email, "groups": [GROUP], "client_id": "claude-code",
            "exp": self.clock.load(Ordering::SeqCst) + 3600,
        })
    }

    async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, String, Option<String>) {
        let mut r = Request::get(path);
        if let Some(t) = token {
            r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = self
            .app
            .clone()
            .oneshot(r.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let challenge = resp
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .map(|v| v.to_str().unwrap().to_string());
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into(), challenge)
    }

    async fn mcp(&self, claims: Value) -> (StatusCode, String, Option<String>) {
        let t = self.idp.mint_jwt(claims, None);
        self.get("/mcp", Some(&t)).await
    }
}

fn db_id(f: &Fixture, handle: &str) -> String {
    let conn = connection::open_registry(&f._dir.path().join("registry.sqlite")).unwrap();
    registry::lookup(&conn, handle)
        .unwrap()
        .unwrap()
        .database_id
}

#[tokio::test]
async fn metadata_names_pocket_id_and_the_mcp_resource() {
    let f = fixture().await;
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let (s, body, _) = f.get(path, None).await;
        assert_eq!(s, StatusCode::OK);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["resource"], RESOURCE);
        assert_eq!(v["authorization_servers"], json!([f.idp.issuer]));
    }
}

#[tokio::test]
async fn no_token_is_401_pointing_at_the_metadata() {
    let f = fixture().await;
    let (s, _, ch) = f.get("/mcp", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        ch.unwrap(),
        "Bearer resource_metadata=\"https://site.test/.well-known/oauth-protected-resource/mcp\""
    );
}

#[tokio::test]
async fn a_valid_token_is_served_as_its_tenant_and_names_the_client() {
    let f = fixture().await;
    let (s, body, _) = f.mcp(f.claims("Alice@test.local")).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body, format!("{}|claude-code", db_id(&f, "alice")));
}

#[tokio::test]
async fn two_tenants_never_cross() {
    let f = fixture().await;
    let (_, a, _) = f.mcp(f.claims("alice@test.local")).await;
    let (_, b, _) = f.mcp(f.claims("bob@test.local")).await;
    assert_eq!(a.split('|').next().unwrap(), db_id(&f, "alice"));
    assert_eq!(b.split('|').next().unwrap(), db_id(&f, "bob"));
    assert_ne!(a, b);
    // Pocket ID's own account for a third person with no binding is nobody's tenant.
    let (s, _, _) = f.mcp(f.claims("carol@test.local")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let _ = &f.tenants;
}

#[tokio::test]
async fn every_bad_token_is_refused_with_an_invalid_token_challenge() {
    let f = fixture().await;
    let now = f.clock.load(Ordering::SeqCst);
    let cases: Vec<(&str, Value, Option<usize>)> = vec![
        ("wrong aud", json!({"aud": "https://site.test/api"}), None),
        (
            "other site's mcp",
            json!({"aud": "https://other.test/mcp"}),
            None,
        ),
        ("wrong iss", json!({"iss": "http://evil.test"}), None),
        ("expired", json!({"exp": now - 3600}), None),
        ("bad signature", json!({}), Some(1)),
    ];
    for (name, patch, sign_with) in cases {
        let mut c = f.claims("alice@test.local");
        for (k, v) in patch.as_object().unwrap() {
            c[k] = v.clone();
        }
        let t = f.idp.mint_jwt(c, sign_with);
        let (s, _, ch) = f.get("/mcp", Some(&t)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{name}");
        assert!(ch.unwrap().contains("error=\"invalid_token\""), "{name}");
    }
    let (s, _, _) = f.get("/mcp", Some("not.a.jwt")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_valid_token_for_a_user_outside_the_group_is_403() {
    let f = fixture().await;
    let mut c = f.claims("alice@test.local");
    c["groups"] = json!(["somebody-else"]);
    assert_eq!(f.mcp(c).await.0, StatusCode::FORBIDDEN);
    let mut c = f.claims("alice@test.local");
    c["groups"] = json!([]);
    assert_eq!(f.mcp(c).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn with_no_groups_claim_the_registry_decides() {
    let f = fixture().await;
    let mut c = f.claims("alice@test.local");
    c.as_object_mut().unwrap().remove("groups");
    assert_eq!(f.mcp(c).await.0, StatusCode::OK);
    let mut c = f.claims("stranger@test.local");
    c.as_object_mut().unwrap().remove("groups");
    assert_eq!(f.mcp(c).await.0, StatusCode::FORBIDDEN);
    let mut c = f.claims("alice@test.local");
    c.as_object_mut().unwrap().remove("groups");
    c.as_object_mut().unwrap().remove("email");
    assert_eq!(f.mcp(c).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_token_is_not_honoured_on_api() {
    let f = fixture().await;
    let t = f.idp.mint_jwt(f.claims("alice@test.local"), None);
    let (s, body, _) = f.get("/api/cards", Some(&t)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(!body.contains("secret"));
}

#[tokio::test]
async fn the_layer_itself_refuses_a_bearer_off_mcp() {
    let f = fixture().await;
    let auth = McpAuth::discover(
        McpAuthConfig {
            issuer: f.idp.issuer.clone(),
            resource: RESOURCE.into(),
            required_group: GROUP.into(),
            clock_skew: std::time::Duration::from_secs(5),
            jwks_refetch_min: std::time::Duration::ZERO,
        },
        f.tenants.clone(),
        &Telemetry::disabled(),
    )
    .await
    .unwrap();
    let app = Router::new()
        .route("/api/cards", get(|| async { "secret" }))
        .layer(from_fn_with_state(auth, McpAuth::layer));
    let t = f.idp.mint_jwt(f.claims("alice@test.local"), None);
    let r = app
        .oneshot(
            Request::get("/api/cards")
                .header(header::AUTHORIZATION, format!("Bearer {t}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_rotated_signing_key_is_picked_up_on_its_first_token() {
    let f = fixture().await;
    assert_eq!(f.mcp(f.claims("alice@test.local")).await.0, StatusCode::OK);
    f.idp.rotate();
    assert_eq!(f.mcp(f.claims("alice@test.local")).await.0, StatusCode::OK);
}
