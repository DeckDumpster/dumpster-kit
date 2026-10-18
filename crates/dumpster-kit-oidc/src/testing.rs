//! A fake OIDC provider on a loopback port: discovery, JWKS (with rotation), and a token endpoint
//! that checks the client secret, the single-use code and the PKCE verifier, then returns an ID
//! token signed with a real RSA key. A consumer's tests drive their app's `/auth/*` routes
//! against it with no bypass.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::RsaPrivateKey;
use rsa::pkcs8::EncodePrivateKey;
use rsa::traits::PublicKeyParts;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;

use crate::config::OidcConfig;

pub const CLIENT_ID: &str = "test-client";
pub const CLIENT_SECRET: &str = "test-secret";
pub const REDIRECT_URL: &str = "http://app.test/auth/callback";
pub const GROUP: &str = "dumpstar-users";

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Key {
    kid: String,
    enc: EncodingKey,
    n: String,
    e: String,
}

/// RSA generation is slow in a debug build; the two keys are made once per process.
fn pool() -> &'static [Key; 2] {
    static POOL: OnceLock<[Key; 2]> = OnceLock::new();
    POOL.get_or_init(|| {
        let mk = |kid: &str| {
            let sk = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
            let pk = sk.to_public_key();
            let pem = sk.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
            Key {
                kid: kid.into(),
                enc: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
                n: B64.encode(pk.n().to_bytes_be()),
                e: B64.encode(pk.e().to_bytes_be()),
            }
        };
        [mk("key-a"), mk("key-b")]
    })
}

/// What the ID token for one sign-in says. `Default` is a valid token for a member of [`GROUP`].
#[derive(Clone)]
pub struct TokenSpec {
    pub sub: String,
    pub email: String,
    pub groups: Vec<String>,
    pub aud: Option<String>,
    pub iss: Option<String>,
    pub exp_in: i64,
    pub iat_offset: i64,
    pub nonce: Option<String>,
    /// Sign with this pool key (0 or 1) instead of the active one.
    pub sign_with: Option<usize>,
}

impl Default for TokenSpec {
    fn default() -> Self {
        TokenSpec {
            sub: "user-1".into(),
            email: "Alice@Example.com".into(),
            groups: vec![GROUP.into()],
            aud: None,
            iss: None,
            exp_in: 3600,
            iat_offset: 0,
            nonce: None,
            sign_with: None,
        }
    }
}

struct Pending {
    challenge: String,
    nonce: String,
    spec: TokenSpec,
}

struct Inner {
    issuer: String,
    active: usize,
    published: Vec<usize>,
    codes: HashMap<String, Pending>,
    jwks_fetches: usize,
    end_sessions: Vec<HashMap<String, String>>,
    counter: u64,
}

pub struct FakeProvider {
    inner: Arc<Mutex<Inner>>,
    pub issuer: String,
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self._task.abort();
    }
}

type St = Arc<Mutex<Inner>>;

impl FakeProvider {
    pub async fn start() -> Self {
        tokio::task::spawn_blocking(pool).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let inner = Arc::new(Mutex::new(Inner {
            issuer: issuer.clone(),
            active: 0,
            published: vec![0],
            codes: HashMap::new(),
            jwks_fetches: 0,
            end_sessions: vec![],
            counter: 0,
        }));
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .route("/logout", get(end_session))
            .with_state(inner.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        FakeProvider {
            inner,
            issuer,
            _task: task,
        }
    }

    /// A config pointed at this provider.
    pub fn config(&self) -> OidcConfig {
        let key = "k".repeat(32).into_bytes();
        OidcConfig {
            issuer: self.issuer.clone(),
            client_id: CLIENT_ID.into(),
            client_secret: CLIENT_SECRET.into(),
            redirect_url: REDIRECT_URL.into(),
            required_group: GROUP.into(),
            cookie_key: key,
            session_ttl: std::time::Duration::from_secs(3600),
            bypass_paths: vec![],
            clock_skew: std::time::Duration::from_secs(60),
            jwks_refetch_min: std::time::Duration::ZERO,
        }
    }

    /// The user "approves" an authorization request: returns the code the browser would carry
    /// back to the callback. `auth_url` is the `Location` of `/auth/login`.
    pub fn authorize(&self, auth_url: &str, spec: TokenSpec) -> String {
        let url = Url::parse(auth_url).expect("authorization url");
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["client_id"], CLIENT_ID);
        let mut g = self.inner.lock().unwrap();
        g.counter += 1;
        let code = format!("code-{}", g.counter);
        g.codes.insert(
            code.clone(),
            Pending {
                challenge: q["code_challenge"].clone(),
                nonce: q["nonce"].clone(),
                spec,
            },
        );
        code
    }

    /// Publish only the other key and sign with it from now on; the old key leaves the JWKS.
    pub fn rotate(&self) {
        let mut g = self.inner.lock().unwrap();
        g.active = 1 - g.active;
        g.published = vec![g.active];
    }

    /// The query of every request the provider's end-session endpoint has served: each is the
    /// provider's own session ending.
    pub fn end_sessions(&self) -> Vec<HashMap<String, String>> {
        self.inner.lock().unwrap().end_sessions.clone()
    }

    pub fn jwks_fetches(&self) -> usize {
        self.inner.lock().unwrap().jwks_fetches
    }
}

async fn discovery(State(s): State<St>) -> impl IntoResponse {
    let iss = s.lock().unwrap().issuer.clone();
    Json(json!({
        "issuer": iss,
        "authorization_endpoint": format!("{iss}/authorize"),
        "token_endpoint": format!("{iss}/token"),
        "jwks_uri": format!("{iss}/jwks"),
        "end_session_endpoint": format!("{iss}/logout"),
    }))
}

async fn jwks(State(s): State<St>) -> impl IntoResponse {
    let mut g = s.lock().unwrap();
    g.jwks_fetches += 1;
    let keys: Vec<_> = g
        .published
        .iter()
        .map(|&i| {
            let k = &pool()[i];
            json!({"kty":"RSA","alg":"RS256","use":"sig","kid":k.kid,"n":k.n,"e":k.e})
        })
        .collect();
    Json(json!({ "keys": keys }))
}

async fn token(State(s): State<St>, Form(f): Form<HashMap<String, String>>) -> impl IntoResponse {
    let fail = || {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_grant"})),
        )
            .into_response()
    };
    let mut g = s.lock().unwrap();
    if f.get("client_id").map(String::as_str) != Some(CLIENT_ID)
        || f.get("client_secret").map(String::as_str) != Some(CLIENT_SECRET)
        || f.get("redirect_uri").map(String::as_str) != Some(REDIRECT_URL)
    {
        return fail();
    }
    let Some(p) = f.get("code").and_then(|c| g.codes.remove(c)) else {
        return fail();
    };
    let verifier = f.get("code_verifier").map(String::as_str).unwrap_or("");
    if B64.encode(Sha256::digest(verifier.as_bytes())) != p.challenge {
        return fail();
    }
    let now = unix_now();
    let sp = p.spec;
    let key = &pool()[sp.sign_with.unwrap_or(g.active)];
    let claims = json!({
        "iss": sp.iss.unwrap_or(g.issuer.clone()),
        "aud": sp.aud.unwrap_or(CLIENT_ID.into()),
        "sub": sp.sub, "email": sp.email, "groups": sp.groups,
        "iat": now + sp.iat_offset, "exp": now + sp.exp_in,
        "nonce": sp.nonce.unwrap_or(p.nonce),
    });
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some(key.kid.clone());
    let id_token = jsonwebtoken::encode(&h, &claims, &key.enc).unwrap();
    Json(json!({"id_token": id_token, "access_token": "x", "token_type": "Bearer"})).into_response()
}

async fn end_session(
    State(s): State<St>,
    axum::extract::Query(q): axum::extract::Query<HashMap<String, String>>,
) -> impl IntoResponse {
    s.lock().unwrap().end_sessions.push(q);
    StatusCode::OK
}
