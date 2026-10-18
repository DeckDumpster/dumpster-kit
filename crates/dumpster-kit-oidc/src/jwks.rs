//! The provider's signing keys: fetched at startup, cached for an hour, re-fetched when a token
//! names an unknown `kid` — but not more often than `refetch_min`, so a fabricated `kid` cannot
//! be used to hammer the provider. A rotated key is picked up on its first token; a key the
//! provider has dropped leaves the cache at the next fetch.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::OidcError;

const TTL: Duration = Duration::from_secs(3600);

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

struct Inner {
    keys: HashMap<String, DecodingKey>,
    fetched: Option<Instant>,
}

pub struct JwksCache {
    url: String,
    client: reqwest::Client,
    refetch_min: Duration,
    inner: Mutex<Inner>,
}

impl JwksCache {
    pub fn new(url: String, client: reqwest::Client, refetch_min: Duration) -> Self {
        JwksCache {
            url,
            client,
            refetch_min,
            inner: Mutex::new(Inner {
                keys: HashMap::new(),
                fetched: None,
            }),
        }
    }

    async fn fetch(&self, inner: &mut Inner) -> Result<(), OidcError> {
        inner.fetched = Some(Instant::now());
        let jwks: Jwks = self
            .client
            .get(&self.url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| OidcError::Provider(format!("jwks fetch: {e}")))?
            .json()
            .await
            .map_err(|e| OidcError::Provider(format!("jwks body: {e}")))?;
        let mut keys = HashMap::new();
        for k in jwks.keys {
            if let (Some(kid), "RSA", Some(n), Some(e)) = (k.kid, k.kty.as_str(), k.n, k.e)
                && let Ok(dk) = DecodingKey::from_rsa_components(&n, &e)
            {
                keys.insert(kid, dk);
            }
        }
        inner.keys = keys;
        Ok(())
    }

    pub async fn prime(&self) -> Result<(), OidcError> {
        let mut g = self.inner.lock().await;
        self.fetch(&mut g).await
    }

    pub async fn get(&self, kid: &str) -> Option<DecodingKey> {
        let mut g = self.inner.lock().await;
        let stale = g.fetched.is_none_or(|t| t.elapsed() > TTL);
        if !stale && let Some(k) = g.keys.get(kid) {
            return Some(k.clone());
        }
        let may = g.fetched.is_none_or(|t| t.elapsed() >= self.refetch_min);
        if (stale || !g.keys.contains_key(kid)) && may {
            let _ = self.fetch(&mut g).await;
        }
        g.keys.get(kid).cloned()
    }
}
