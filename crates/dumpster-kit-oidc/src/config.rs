//! Configuration, read from the environment.
//!
//! Variables are `<PREFIX>_OIDC_*` for the application's prefix (`DUMPSTAR`, `PKDUMP`, …):
//!
//! * `ISSUER` — e.g. `https://id.gantt.pub`; discovery is fetched from
//!   `<issuer>/.well-known/openid-configuration`
//! * `CLIENT_ID`, `CLIENT_SECRET` — the app's Pocket ID OIDC client
//! * `REDIRECT_URL` — the absolute URL of `/auth/callback` on this site
//! * `REQUIRED_GROUP` — the Pocket ID group a user must be in (e.g. `dumpstar-users`)
//! * `COOKIE_KEY` — at least 32 bytes, hex or base64, signing the session cookie
//! * `SESSION_TTL_SECS` — optional, default [`DEFAULT_SESSION_TTL`] (30 days): the one session
//!   lifetime setting. The session is sliding: any request made more than a day after it was
//!   issued renews it for another full ttl, so a user who does not visit for this long signs in
//!   again. Separately, every session ends [`MAX_SESSION_AGE`] (90 days) after sign-in, however
//!   active the user is.
//! * `BYPASS_PATHS` — optional, comma-separated path prefixes served without a session (the
//!   machine paths guarded by Cloudflare service tokens)
//!
//! The group rule: the session carries the `groups` claim captured at sign-in, and the current
//! `REQUIRED_GROUP` is checked against that snapshot on every request, so changing
//! `REQUIRED_GROUP` takes effect at once. Removing a user from the group in Pocket ID takes
//! effect only when the user next signs in, which the 90-day cap bounds. Evicting a removed
//! user at once takes rotating the site's `COOKIE_KEY`, which signs everyone out.
//!
//! Nothing has a default except the two optional ones: a missing variable is a startup error
//! naming every missing one.

use std::time::Duration;

use base64::Engine;

use crate::error::OidcError;

pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(30 * 86_400);

/// A session ends this long after sign-in, however often it is renewed.
pub const MAX_SESSION_AGE: Duration = Duration::from_secs(90 * 86_400);

#[derive(Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
    pub required_group: String,
    pub cookie_key: Vec<u8>,
    pub session_ttl: Duration,
    pub bypass_paths: Vec<String>,
    /// Leeway applied to `exp` and `iat`.
    pub clock_skew: Duration,
    /// Minimum gap between JWKS re-fetches triggered by an unknown `kid`.
    pub jwks_refetch_min: Duration,
}

impl std::fmt::Debug for OidcConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("required_group", &self.required_group)
            .finish_non_exhaustive()
    }
}

impl OidcConfig {
    pub fn from_env(prefix: &str) -> Result<Self, OidcError> {
        Self::from_lookup(prefix, |k| std::env::var(k).ok())
    }

    pub fn from_lookup(
        prefix: &str,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, OidcError> {
        let var = |s: &str| format!("{prefix}_OIDC_{s}");
        let mut missing = Vec::new();
        let mut req = |s: &str| match get(&var(s)).filter(|v| !v.trim().is_empty()) {
            Some(v) => v.trim().to_string(),
            None => {
                missing.push(var(s));
                String::new()
            }
        };
        let issuer = req("ISSUER");
        let client_id = req("CLIENT_ID");
        let client_secret = req("CLIENT_SECRET");
        let redirect_url = req("REDIRECT_URL");
        let required_group = req("REQUIRED_GROUP");
        let key_raw = req("COOKIE_KEY");
        if !missing.is_empty() {
            return Err(OidcError::Config(format!(
                "missing environment: {}",
                missing.join(", ")
            )));
        }
        let cookie_key = decode_key(&key_raw).ok_or_else(|| {
            OidcError::Config(format!("{} must be hex or base64", var("COOKIE_KEY")))
        })?;
        if cookie_key.len() < 32 {
            return Err(OidcError::Config(format!(
                "{} must be at least 32 bytes",
                var("COOKIE_KEY")
            )));
        }
        let session_ttl = match get(&var("SESSION_TTL_SECS")) {
            Some(v) => Duration::from_secs(v.trim().parse().map_err(|_| {
                OidcError::Config(format!("{} must be whole seconds", var("SESSION_TTL_SECS")))
            })?),
            None => DEFAULT_SESSION_TTL,
        };
        let bypass_paths = get(&var("BYPASS_PATHS"))
            .map(|v| {
                v.split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(OidcConfig {
            issuer: issuer.trim_end_matches('/').to_string(),
            client_id,
            client_secret,
            redirect_url,
            required_group,
            cookie_key,
            session_ttl,
            bypass_paths,
            clock_skew: Duration::from_secs(60),
            jwks_refetch_min: Duration::from_secs(60),
        })
    }
}

fn decode_key(s: &str) -> Option<Vec<u8>> {
    if s.len().is_multiple_of(2) && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect();
    }
    let e = base64::engine::general_purpose::STANDARD;
    let u = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    e.decode(s)
        .or_else(|_| u.decode(s.trim_end_matches('=')))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(extra: &[(&str, &str)]) -> HashMap<String, String> {
        let key = "ab".repeat(32);
        let mut m: HashMap<String, String> = HashMap::new();
        let base = "ISSUER=https://id.example/ CLIENT_ID=cid CLIENT_SECRET=sec \
             REDIRECT_URL=https://app.example/auth/callback REQUIRED_GROUP=dumpstar-users";
        for pair in base.split_whitespace() {
            let (k, v) = pair.split_once('=').unwrap();
            m.insert(format!("DUMPSTAR_OIDC_{k}"), v.to_string());
        }
        m.insert("DUMPSTAR_OIDC_COOKIE_KEY".into(), key);
        for (k, v) in extra {
            m.insert(format!("DUMPSTAR_OIDC_{k}"), v.to_string());
        }
        m
    }

    #[test]
    fn reads_everything() {
        let m = env(&[
            ("BYPASS_PATHS", "/api/m, /hook"),
            ("SESSION_TTL_SECS", "60"),
        ]);
        let c = OidcConfig::from_lookup("DUMPSTAR", |k| m.get(k).cloned()).unwrap();
        assert_eq!(c.issuer, "https://id.example");
        assert_eq!(c.bypass_paths, vec!["/api/m", "/hook"]);
        assert_eq!(c.session_ttl, Duration::from_secs(60));
        assert_eq!(c.cookie_key.len(), 32);
    }

    #[test]
    fn session_ttl_defaults_to_30_days_under_a_90_day_cap() {
        let m = env(&[]);
        let c = OidcConfig::from_lookup("DUMPSTAR", |k| m.get(k).cloned()).unwrap();
        assert_eq!(c.session_ttl, Duration::from_secs(30 * 86_400));
        assert_eq!(MAX_SESSION_AGE, Duration::from_secs(90 * 86_400));
    }

    #[test]
    fn names_every_missing_variable() {
        let e = OidcConfig::from_lookup("DUMPSTAR", |_| None)
            .unwrap_err()
            .to_string();
        for s in "ISSUER CLIENT_ID CLIENT_SECRET REDIRECT_URL REQUIRED_GROUP COOKIE_KEY".split(' ')
        {
            assert!(e.contains(&format!("DUMPSTAR_OIDC_{s}")), "{e}");
        }
    }

    #[test]
    fn short_key_is_refused() {
        let m = env(&[("COOKIE_KEY", "abcd")]);
        assert!(OidcConfig::from_lookup("DUMPSTAR", |k| m.get(k).cloned()).is_err());
    }
}
