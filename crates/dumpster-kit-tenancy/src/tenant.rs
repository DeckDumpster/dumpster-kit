//! Tenant resolution — mapping one HTTP request to exactly one tenant's collection database.
//!
//! # Resolution is driven by a verified identity
//!
//! [`Tenants::resolve`] takes a [`VerifiedIdentity`] — a proof-carrying type whose constructor is
//! private to this crate. The only source is [`crate::access::layer`], which verified a
//! Cloudflare Access JWT. The email from that identity is looked up in the user registry; if it
//! is bound to an active tenant, that tenant's database is served.
//!
//! # The email is a lookup key, not a filename
//!
//! What the identity carries is an **email address**. What a request is served from is
//! `tenants/<database_id>.sqlite`, and the two are joined by a row in the user registry —
//! never by string equality. Resolution is a `SELECT` with the email as a bound parameter, and
//! the only string that reaches a path constructor is the `database_id` the registry hands back.
//! An email with no binding is a 403; there is nothing for it to escape.
//!
//! # How isolation is enforced
//!
//! Structurally, not by filtering. A tenant's connection is opened against that tenant's own
//! file, so there is no row belonging to another tenant anywhere in its scope — no
//! `WHERE tenant_id = ?` to forget. Four things keep that true:
//!
//! 1. [`Tenants`] is the only holder of connections. The only way to name a database is a
//!    [`TenantId`], which has no public constructor: only [`Tenants::resolve`] mints one.
//! 2. The resolved id lives in a task-local for the duration of the request ([`layer`]); a
//!    handler that forgets to thread the tenant through cannot exist, because nothing is threaded.
//! 3. [`crate::connection::assert_isolated`] fails the open unless the connection is wired to
//!    exactly this tenant's file plus the one read-only shared catalog.
//! 4. A request cannot reach resolution without a [`VerifiedIdentity`], and a valid Access JWT
//!    is the only source of one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use rusqlite::Connection;

use crate::access::VerifiedIdentity;
use crate::app::AppPrefix;
use crate::connection::{self, TenantDbSpec};
use crate::error::{Result, TenancyError};
use crate::http::HttpError;
use crate::registry::{self, UserState};

/// A tenant that has been resolved for the request in hand.
///
/// The inner value is private and there is no public constructor: the only way to obtain one is
/// [`Tenants::resolve`]. So a route cannot name a tenant of its own choosing — including its own,
/// including a default one.
///
/// What it holds is *storage*, not identity: the `database_id` the registry issued. It is never
/// the handle a request sent — that string stops at the registry lookup.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TenantId(String);

impl TenantId {
    /// The identifier of the database this request is served from.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The set of collection databases this process may serve, and the open connections to them.
///
/// The registry connection is opened once and held: it is consulted on every request. One
/// collection connection per tenant is opened on first use and kept for the life of the process,
/// each behind its own mutex — so two tenants' requests do not serialise against each other.
pub struct Tenants {
    app: AppPrefix,
    dir: PathBuf,
    registry: Mutex<Connection>,
    shared_db: PathBuf,
    spec: TenantDbSpec,
    open: Mutex<HashMap<TenantId, Arc<Mutex<Connection>>>>,
}

impl Tenants {
    /// Serve whichever user each request's verified identity is bound to, resolving the email
    /// through the registry at `registry_db` to a database under `tenants_dir`.
    ///
    /// No *collection* is created here or by [`Tenants::resolve`]: a user exists because an
    /// operator registered them and made their database, and an identity bound to no one gets a
    /// 403. The registry itself is opened — and its schema applied — up front, so a data root
    /// that has never had a user comes up and answers 403 rather than failing the first request.
    pub fn new(
        app: AppPrefix,
        tenants_dir: PathBuf,
        shared_db: PathBuf,
        registry_db: &Path,
        spec: TenantDbSpec,
    ) -> Result<Self> {
        Ok(Tenants {
            app,
            dir: tenants_dir,
            registry: Mutex::new(connection::open_registry(registry_db)?),
            shared_db,
            spec,
            open: Mutex::new(HashMap::new()),
        })
    }

    /// The tenant an OIDC-signed-in user is served as; resolves exactly as [`Tenants::resolve`]
    /// does, by the session's email.
    pub fn resolve_session(
        &self,
        user: &dumpster_kit_oidc::SessionUser,
    ) -> std::result::Result<TenantId, HttpError> {
        let id = self.resolve(&VerifiedIdentity::from_session(user))?;
        // A verified sign-in is where the site learns which `sub` is this email, so a token that
        // carries only `sub` can resolve later. Best effort: a failure here never refuses a user.
        if let Ok(conn) = self.registry.lock() {
            let _ = registry::learn_sub(&conn, &user.email, &user.sub);
        }
        Ok(id)
    }

    /// The tenant a verified provider subject is bound to, for a token that carries `sub` and no
    /// email. The subject must have been learned at a browser sign-in ([`Tenants::resolve_session`]);
    /// an unknown one is a 403 telling the user to sign in to the site once.
    pub fn resolve_subject(
        &self,
        sub: &str,
        issuer: &str,
    ) -> std::result::Result<TenantId, HttpError> {
        let found = self
            .registry
            .lock()
            .map_err(|_| HttpError::internal("registry mutex poisoned"))
            .and_then(|conn| {
                registry::lookup_by_sub(&conn, sub, issuer)
                    .map_err(|e| HttpError::internal(format!("registry lookup failed: {e}")))
            })?;
        match found {
            Some(user) if user.state == UserState::Active => self.tenant_id_for(user.database_id),
            _ => Err(HttpError(
                StatusCode::FORBIDDEN,
                "this account is not known to the site yet; sign in to the site once in a browser, then retry"
                    .into(),
            )),
        }
    }

    /// Follow a learned `sub` onto the active tenant now bound to the email it was learned for
    /// ([`registry::rebind_sub`]). Best effort; returns whether a binding was updated.
    pub fn rebind_subject(&self, sub: &str, issuer: &str) -> bool {
        self.registry
            .lock()
            .ok()
            .and_then(|conn| registry::rebind_sub(&conn, sub, issuer).ok())
            .unwrap_or(false)
    }

    /// Like [`Tenants::serve_session`], for a token with no email: the tenant is resolved by `sub`.
    pub async fn serve_subject<F: std::future::Future>(
        &self,
        sub: &str,
        issuer: &str,
        f: F,
    ) -> std::result::Result<F::Output, HttpError> {
        let id = self.resolve_subject(sub, issuer)?;
        Ok(CURRENT.scope(id, f).await)
    }

    /// Run `f` as the tenant `user` is bound to, for a bearer-token layer that has verified the
    /// token itself (`dumpster-kit-mcp-auth`). The registry decides, exactly as for a browser
    /// session: no active binding is a 403 and `f` never runs.
    pub async fn serve_session<F: std::future::Future>(
        &self,
        user: &dumpster_kit_oidc::SessionUser,
        f: F,
    ) -> std::result::Result<F::Output, HttpError> {
        let id = self.resolve_session(user)?;
        Ok(CURRENT.scope(id, f).await)
    }

    /// The tenant this identity is served as.
    ///
    /// The identity's **email** is compared against `user_identity.email` as a bound parameter and
    /// is then done with. No active binding for it is a **403** naming the command to add one;
    /// bound, the result carries the `database_id` on their row, never the email.
    pub fn resolve(&self, identity: &VerifiedIdentity) -> std::result::Result<TenantId, HttpError> {
        let email = identity.email();
        let found = self
            .registry
            .lock()
            .map_err(|_| HttpError::internal("registry mutex poisoned"))
            .and_then(|conn| {
                registry::lookup_by_email(&conn, email)
                    .map_err(|e| HttpError::internal(format!("registry lookup failed: {e}")))
            })?;
        let database_id = match found {
            Some(user) if user.state == UserState::Active => user.database_id,
            _ => {
                return Err(HttpError(
                    StatusCode::FORBIDDEN,
                    format!(
                        "no tenant is bound to identity {email:?}; \
                         run: {} tenant identity add <handle> --email {email}",
                        self.app.cli()
                    ),
                ));
            }
        };
        self.tenant_id_for(database_id)
    }

    fn tenant_id_for(&self, database_id: String) -> std::result::Result<TenantId, HttpError> {
        // Registered, so their database must be on disk. It is not created here: a resolver that
        // opened whatever it was handed would let a caller provision by guessing, and a registry
        // that disagrees with the disk is a fault to surface, not to paper over with an empty
        // collection.
        let db = crate::paths::tenant_db_file(&self.dir, &database_id)
            .map_err(|e| HttpError::internal(e.to_string()))?;
        if !db.exists() {
            return Err(HttpError::internal(format!(
                "registry names database {database_id} for this tenant, but {} does not exist",
                db.display()
            )));
        }
        Ok(TenantId(database_id))
    }

    /// The open connection for `id`, opening it if this is its first request. Always through
    /// [`connection::connect_tenant`], so always asserted isolated.
    pub fn connection(&self, id: &TenantId) -> Result<Arc<Mutex<Connection>>> {
        let mut open = self
            .open
            .lock()
            .map_err(|_| TenancyError::Env("tenant connection map poisoned".into()))?;
        if let Some(conn) = open.get(id) {
            return Ok(conn.clone());
        }
        // `tenant_db_file` and not an interpolation: it refuses anything that is not a ULID the
        // registry minted, so the last step before a path exists is a check that the string came
        // from us.
        let path = crate::paths::tenant_db_file(&self.dir, id.as_str())?;
        let conn = connection::connect_tenant(&path, &self.shared_db, &self.spec)?;
        let handle = Arc::new(Mutex::new(conn));
        open.insert(id.clone(), handle.clone());
        Ok(handle)
    }
}

tokio::task_local! {
    /// The tenant resolved for the request being served on this task.
    static CURRENT: TenantId;
}

/// The tenant this request resolved to.
///
/// Absent means the request never went through [`layer`], which for anything touching a database
/// is a wiring bug, not a case to paper over with a default — a default here would silently
/// serve one tenant's collection to another.
pub fn current() -> std::result::Result<TenantId, HttpError> {
    CURRENT.try_with(TenantId::clone).map_err(|_| {
        HttpError::internal(
            "no tenant resolved for this request — the route is mounted outside the \
             tenant-resolution layer",
        )
    })
}

/// Middleware: resolve the tenant from the request's verified identity, then run the rest of the
/// request with it in scope. Mount it *inside* [`crate::access::layer`] (it needs the identity
/// that layer puts in scope), with
/// `axum::middleware::from_fn_with_state(tenants, tenant::layer)`.
pub async fn layer(
    State(tenants): State<Arc<Tenants>>,
    request: Request,
    next: Next,
) -> std::result::Result<Response, HttpError> {
    let identity = crate::access::current()?;
    let id = tenants.resolve(&identity)?;
    Ok(CURRENT.scope(id, next.run(request)).await)
}

/// Run `f` as if the request had resolved to `tenant`. For a consumer's tests that exercise code
/// reading [`current`] without going through the router. The only way to build a [`TenantId`]
/// outside [`Tenants::resolve`], and only under the `testing` feature.
#[cfg(any(test, feature = "testing"))]
pub async fn as_tenant<F: std::future::Future>(database_id: &str, f: F) -> F::Output {
    CURRENT.scope(TenantId(database_id.to_string()), f).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_version::SchemaSpec;
    use crate::testing::identity;

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

    fn catalog(dir: &Path) -> PathBuf {
        let path = dir.join("shared.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE cards (id INTEGER PRIMARY KEY); PRAGMA user_version = 1;")
            .unwrap();
        path
    }

    /// A multi-tenant instance with `handles` registered, each with the collection database its
    /// `database_id` names and an email binding `<handle>@test.local`.
    fn provisioned(dir: &Path, handles: &[&str]) -> (Tenants, PathBuf, Connection) {
        let shared = catalog(dir);
        let tenants_dir = dir.join("tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let registry_db = dir.join("registry.sqlite");
        let reg = connection::open_registry(&registry_db).unwrap();
        for handle in handles {
            let user = registry::insert(&reg, handle).unwrap();
            connection::open_tenant(
                &crate::paths::tenant_db_file(&tenants_dir, &user.database_id).unwrap(),
                &SPEC,
            )
            .unwrap();
            registry::identity_add(
                &reg,
                &user.database_id,
                &format!("{handle}@test.local"),
                None,
                None,
            )
            .unwrap();
        }
        let tenants = Tenants::new(
            AppPrefix::new("KITTEST").unwrap(),
            tenants_dir.clone(),
            shared,
            &registry_db,
            SPEC,
        )
        .unwrap();
        (tenants, tenants_dir, reg)
    }

    fn status(result: std::result::Result<TenantId, HttpError>) -> StatusCode {
        result.expect_err("resolved when it should not have").0
    }

    #[test]
    fn an_oidc_session_user_resolves_by_email_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, _tenants_dir, _reg) = provisioned(dir.path(), &["alice"]);
        let session = |email: &str| dumpster_kit_oidc::SessionUser {
            sub: "oidc-sub".into(),
            email: email.into(),
            name: "A".into(),
            groups: vec![],
            exp: 0,
        };
        assert!(
            tenants
                .resolve_session(&session("Alice@Test.Local"))
                .is_ok()
        );
        assert_eq!(
            status(tenants.resolve_session(&session("nobody@test.local"))),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn an_unbound_email_is_forbidden_and_names_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, _reg) = provisioned(dir.path(), &[]);
        let HttpError(status, body) = tenants
            .resolve(&identity("unknown@example.com"))
            .unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body.contains("kittest tenant identity add"),
            "unhelpful error: {body}"
        );
        assert_eq!(
            std::fs::read_dir(&tenants_dir).unwrap().count(),
            0,
            "resolution created something"
        );
    }

    /// A real file in `tenants/` with no registry row is unreachable.
    #[test]
    fn a_database_the_registry_does_not_name_cannot_be_reached() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &["alice"]);
        connection::open_tenant(&tenants_dir.join("ghost.sqlite"), &SPEC).unwrap();
        assert_eq!(
            status(tenants.resolve(&identity("ghost@test.local"))),
            StatusCode::FORBIDDEN
        );

        let alice = registry::lookup(&reg, "alice").unwrap().unwrap();
        assert_eq!(
            tenants
                .resolve(&identity("alice@test.local"))
                .unwrap()
                .as_str(),
            alice.database_id
        );
    }

    #[test]
    fn a_detached_tenants_email_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &["alice"]);
        let alice = registry::lookup(&reg, "alice").unwrap().unwrap();
        registry::detach(&reg, "alice").unwrap();
        assert!(
            crate::paths::tenant_db_file(&tenants_dir, &alice.database_id)
                .unwrap()
                .exists()
        );
        assert_eq!(
            status(tenants.resolve(&identity("alice@test.local"))),
            StatusCode::FORBIDDEN
        );
    }

    /// Re-registering a released handle serves the new user their own empty database, never
    /// their predecessor's.
    #[test]
    fn a_recycled_handle_does_not_inherit_its_predecessors_database() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &["alice"]);
        let first = registry::lookup(&reg, "alice").unwrap().unwrap();
        let first_conn = tenants
            .connection(&tenants.resolve(&identity("alice@test.local")).unwrap())
            .unwrap();
        first_conn
            .lock()
            .unwrap()
            .execute("INSERT INTO binders (name) VALUES ('first alice')", [])
            .unwrap();
        registry::detach(&reg, "alice").unwrap();

        let second = registry::insert(&reg, "alice").unwrap();
        assert_ne!(second.database_id, first.database_id);
        connection::open_tenant(
            &crate::paths::tenant_db_file(&tenants_dir, &second.database_id).unwrap(),
            &SPEC,
        )
        .unwrap();
        registry::identity_add(&reg, &second.database_id, "alice2@test.local", None, None).unwrap();

        let resolved = tenants.resolve(&identity("alice2@test.local")).unwrap();
        assert_eq!(resolved.as_str(), second.database_id);
        assert_ne!(resolved.as_str(), first.database_id);
        let seen: i64 = tenants
            .connection(&resolved)
            .unwrap()
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM binders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seen, 0, "the new alice saw the old alice's binder");
    }

    #[test]
    fn a_registered_user_with_no_database_is_an_error_not_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, tenants_dir, reg) = provisioned(dir.path(), &[]);
        let alice = registry::insert(&reg, "alice").unwrap();
        registry::identity_add(&reg, &alice.database_id, "alice@test.local", None, None).unwrap();
        assert_eq!(
            status(tenants.resolve(&identity("alice@test.local"))),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert!(
            !crate::paths::tenant_db_file(&tenants_dir, &alice.database_id)
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn each_tenant_gets_its_own_database() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, _tenants_dir, _reg) = provisioned(dir.path(), &["alice", "bob"]);
        let alice = tenants.resolve(&identity("alice@test.local")).unwrap();
        let bob = tenants.resolve(&identity("bob@test.local")).unwrap();
        assert_ne!(alice, bob);

        let a = tenants.connection(&alice).unwrap();
        let b = tenants.connection(&bob).unwrap();
        a.lock()
            .unwrap()
            .execute("INSERT INTO binders (name) VALUES ('alice')", [])
            .unwrap();
        let seen: i64 = b
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM binders", [], |r| r.get(0))
            .unwrap();
        assert_eq!(seen, 0, "bob's connection saw alice's binder");
        assert!(Arc::ptr_eq(&a, &tenants.connection(&alice).unwrap()));
    }

    #[test]
    fn a_foreign_id_has_no_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (tenants, _tenants_dir, _reg) = provisioned(dir.path(), &["alice"]);
        assert!(tenants.connection(&TenantId("alice".into())).is_err());
    }

    #[tokio::test]
    async fn current_is_the_scoped_tenant_and_an_error_outside_it() {
        assert_eq!(current().unwrap_err().0, StatusCode::INTERNAL_SERVER_ERROR);
        let id = crate::registry::mint_database_id();
        let seen = as_tenant(&id, async { current().unwrap() }).await;
        assert_eq!(seen.as_str(), id);
    }

    /// The whole stack: Access verifies, the tenant layer resolves, the handler reads its tenant
    /// and connection — and a token for an unbound email never reaches the handler.
    #[tokio::test]
    async fn the_full_stack_serves_the_tenant_the_token_names() {
        use crate::testing::TestAccessFixture;
        use tower::ServiceExt;

        async fn handler(
            State(tenants): State<Arc<Tenants>>,
        ) -> std::result::Result<String, HttpError> {
            let conn = tenants
                .connection(&current()?)
                .map_err(|e| HttpError::internal(e.to_string()))?;
            let conn = conn.lock().unwrap();
            let names: Vec<String> = conn
                .prepare("SELECT name FROM binders ORDER BY id")
                .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
                .map_err(|e| HttpError::internal(e.to_string()))?;
            Ok(names.join(","))
        }

        let dir = tempfile::tempdir().unwrap();
        let (tenants, _tenants_dir, _reg) = provisioned(dir.path(), &["alice", "bob"]);
        let tenants = Arc::new(tenants);
        for (who, name) in [("alice", "a-binder"), ("bob", "b-binder")] {
            let id = tenants
                .resolve(&identity(&format!("{who}@test.local")))
                .unwrap();
            tenants
                .connection(&id)
                .unwrap()
                .lock()
                .unwrap()
                .execute("INSERT INTO binders (name) VALUES (?1)", [name])
                .unwrap();
        }
        let fx = TestAccessFixture::new().await;
        let app = axum::Router::new()
            .route("/api/binders", axum::routing::get(handler))
            .route_layer(axum::middleware::from_fn_with_state(tenants.clone(), layer))
            .route_layer(axum::middleware::from_fn_with_state(
                fx.access.clone(),
                crate::access::layer,
            ))
            .with_state(tenants.clone());

        let get = |token: String| {
            let app = app.clone();
            async move {
                let resp = app
                    .oneshot(
                        axum::http::Request::get("/api/binders")
                            .header(crate::access::JWT_HEADER, token)
                            .body(axum::body::Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                let status = resp.status();
                let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
                (status, String::from_utf8(body.to_vec()).unwrap())
            }
        };
        assert_eq!(
            get(fx.valid_token("alice@test.local")).await,
            (StatusCode::OK, "a-binder".into())
        );
        assert_eq!(
            get(fx.valid_token("BOB@test.local")).await,
            (StatusCode::OK, "b-binder".into())
        );
        assert_eq!(
            get(fx.valid_token("mallory@test.local")).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            get(fx.expired_token("alice@test.local")).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
}
