//! Signed cookie values: `base64url(payload).base64url(hmac_sha256(key, name | payload))`.
//! The name is inside the MAC so one cookie's value cannot be replayed as another's.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hmac::{Hmac, Mac};
use sha2::Sha256;

#[derive(Clone)]
pub struct Sealer(Vec<u8>);

impl Sealer {
    pub fn new(key: Vec<u8>) -> Self {
        Sealer(key)
    }

    fn mac(&self, name: &str, payload: &[u8]) -> Hmac<Sha256> {
        let mut m = Hmac::<Sha256>::new_from_slice(&self.0).expect("hmac takes any key length");
        m.update(name.as_bytes());
        m.update(b"|");
        m.update(payload);
        m
    }

    pub fn seal(&self, name: &str, payload: &[u8]) -> String {
        let tag = self.mac(name, payload).finalize().into_bytes();
        format!("{}.{}", B64.encode(payload), B64.encode(tag))
    }

    pub fn open(&self, name: &str, value: &str) -> Option<Vec<u8>> {
        let (p, t) = value.split_once('.')?;
        let payload = B64.decode(p).ok()?;
        let tag = B64.decode(t).ok()?;
        self.mac(name, &payload).verify_slice(&tag).ok()?;
        Some(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_tampering_and_renaming() {
        let s = Sealer::new(vec![7; 32]);
        let v = s.seal("a", b"hello");
        assert_eq!(s.open("a", &v).unwrap(), b"hello");
        assert!(s.open("b", &v).is_none());
        assert!(Sealer::new(vec![8; 32]).open("a", &v).is_none());
        let (p, t) = v.split_once('.').unwrap();
        let forged = format!("{}.{t}", B64.encode(b"hellO"));
        assert!(s.open("a", &forged).is_none());
        assert!(s.open("a", p).is_none());
    }
}
