//! The sealed envelope every object in the tenant zone is wrapped in.
//!
//! ```text
//! ┌────────────┬───────────┬───────────────────────────────┐
//! │ "PKDTZ1\n" │ nonce(12) │ AES-256-GCM(parquet) ‖ tag(16)│
//! └────────────┴───────────┴───────────────────────────────┘
//! ```
//!
//! Whole-file, not Parquet's own modular encryption: the tenant zone's
//! deletion story is crypto-shredding (`pd-ulds`), and "every byte of this
//! object is unreadable once the key is gone" is a claim that survives a
//! reader nobody has written yet. Modular encryption leaves a readable
//! skeleton and a second key-management contract to keep in step with the
//! first; it can be revisited if a reader ever needs to push predicates down
//! into a part, which nothing does.
//!
//! ## The nonce is derived, and that is the point
//!
//! A random nonce would make shipping the same part twice write **different
//! bytes to the same key** — content-idempotent but not observably so, and
//! "the zone is unchanged by the second run" would have to become "the zone
//! decrypts to the same thing", which is a much weaker thing to be able to
//! check. So the nonce is
//!
//! ```text
//! SHA-256("dumpstar/tenant-zone/nonce/v1" ‖ 0x00 ‖ object_key ‖ 0x00 ‖ plaintext)[..12]
//! ```
//!
//! The hazard a derived nonce usually carries is reuse across *different*
//! plaintexts under one key, which breaks GCM catastrophically. It cannot
//! arise here: the nonce is a function of the plaintext, so two parts share a
//! nonce only when they are byte-identical, which is the case where sharing
//! one is harmless by construction. (This is the synthetic-IV shape, with
//! SHA-256 standing in for the PRF; the key never enters the hash, so the
//! nonce leaks nothing but equality — and equality of parts is already
//! visible in the object key.)
//!
//! ## The object key is associated data
//!
//! The key string is passed as GCM's AAD, so a part authenticates only under
//! the prefix it was written to. Moving an object into another tenant's
//! partition — or into another date, or renaming its range — makes it fail to
//! open rather than decrypt into the wrong tenant's holdings. That is the
//! property that keeps a bucket-level mistake from becoming a data leak, and
//! it costs nothing.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use dumpster_kit_keys::TenantKey;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::{Result, ShipError};

/// The envelope's magic and version. A byte on the front so an operator with
/// `head -c 7` can tell what they are looking at, and so a format change is a
/// refusal rather than a wrong answer.
pub const MAGIC: &[u8; 7] = b"PKDTZ1\n";

/// Domain separator for the nonce derivation. Versioned with the envelope and rooted at the
/// key's app label, so a site's retried part is byte-identical to the one it wrote before.
fn nonce_domain(label: &str) -> Vec<u8> {
    format!("{label}/tenant-zone/nonce/v1").into_bytes()
}

/// GCM's nonce width.
const NONCE_LEN: usize = 12;

/// Seal `plaintext` for `object_key` under `key`.
///
/// Deterministic: the same three inputs always produce the same bytes.
pub fn seal(key: &TenantKey, object_key: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    let nonce = nonce_for(key.label(), object_key, plaintext);
    let cipher = cipher(key);
    let sealed = cipher
        .encrypt(
            &nonce_of(&nonce[..]),
            Payload {
                msg: plaintext,
                aad: object_key.as_bytes(),
            },
        )
        .map_err(|_| ShipError::Zone(format!("could not seal {object_key}")))?;

    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + sealed.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce[..]);
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// Open an object sealed by [`seal`].
///
/// Fails for the wrong key, for a tampered object, and for one read from a
/// key other than the one it was written to.
pub fn open(key: &TenantKey, object_key: &str, sealed: &[u8]) -> Result<Vec<u8>> {
    let bad = |detail: &str| ShipError::Ciphertext {
        key: object_key.to_string(),
        detail: detail.to_string(),
    };

    if sealed.len() < MAGIC.len() + NONCE_LEN {
        return Err(bad("too short to be a sealed tenant-zone object"));
    }
    if &sealed[..MAGIC.len()] != MAGIC {
        return Err(bad(
            "does not start with the tenant-zone envelope magic — not an object this \
             build wrote",
        ));
    }
    let nonce = &sealed[MAGIC.len()..MAGIC.len() + NONCE_LEN];
    let body = &sealed[MAGIC.len() + NONCE_LEN..];

    cipher(key)
        .decrypt(
            &nonce_of(nonce),
            Payload {
                msg: body,
                aad: object_key.as_bytes(),
            },
        )
        .map_err(|_| {
            bad(
                "did not authenticate — the wrong tenant's key, a modified object, or one \
                 that has been moved to a different key",
            )
        })
}

fn cipher(key: &TenantKey) -> Aes256Gcm {
    Aes256Gcm::new_from_slice(key.as_bytes())
        .expect("a derived tenant key is exactly AES-256's key length")
}

/// A [`NONCE_LEN`]-byte slice as GCM's nonce type.
fn nonce_of(bytes: &[u8]) -> Nonce<aes_gcm::aead::consts::U12> {
    Nonce::try_from(bytes).expect("every caller passes NONCE_LEN bytes")
}

/// The synthetic nonce. See the module docs for why it is not random.
fn nonce_for(label: &str, object_key: &str, plaintext: &[u8]) -> Zeroizing<[u8; NONCE_LEN]> {
    let mut hasher = Sha256::new();
    hasher.update(nonce_domain(label));
    hasher.update([0u8]);
    hasher.update(object_key.as_bytes());
    hasher.update([0u8]);
    hasher.update(plaintext);
    let digest = hasher.finalize();
    let mut out = Zeroizing::new([0u8; NONCE_LEN]);
    out.copy_from_slice(&digest[..NONCE_LEN]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvGuard;

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";
    const KEY_A: &str = "tenant/database_id=01J0000000000000000000000A/dataset=holdings/\
                         as_of=2026-08-14/part-seq-000000000001-000000000009.parquet.enc";

    /// Two registered tenants on one master key, which is the arrangement the
    /// zone actually has.
    fn keys() -> (tempfile::TempDir, EnvGuard, TenantKey, TenantKey) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.key");
        dumpster_kit_keys::master::create_at(&crate::test_support::env(), &path).unwrap();
        let guard = EnvGuard::set(&path);

        let conn = crate::test_support::registry(&[A, B]);
        let a = dumpster_kit_keys::tenant_key(&crate::test_support::env(), &conn, A).unwrap();
        let b = dumpster_kit_keys::tenant_key(&crate::test_support::env(), &conn, B).unwrap();
        (dir, guard, a, b)
    }

    #[test]
    fn a_sealed_object_opens_again() {
        let (_d, _g, a, _b) = keys();
        let sealed = seal(&a, KEY_A, b"PAR1 pretend parquet").unwrap();
        assert_eq!(open(&a, KEY_A, &sealed).unwrap(), b"PAR1 pretend parquet");
    }

    /// The claim the bead asks for in as many words: what lands is not the
    /// plaintext.
    #[test]
    fn the_sealed_bytes_are_not_the_plaintext() {
        let (_d, _g, a, _b) = keys();
        let plaintext = br#"{"printing_id":"sv3pt5-1-normal"}"#;
        let sealed = seal(&a, KEY_A, plaintext).unwrap();
        assert!(
            !sealed
                .windows(plaintext.len())
                .any(|w| w == plaintext.as_slice()),
            "the plaintext is readable in the sealed object"
        );
        assert!(
            !sealed.windows(11).any(|w| w == b"printing_id"),
            "a field name is readable in the sealed object"
        );
    }

    /// Idempotence rests on this: shipping the same part twice writes the
    /// same bytes, not merely equivalent ones.
    #[test]
    fn sealing_is_deterministic() {
        let (_d, _g, a, _b) = keys();
        let once = seal(&a, KEY_A, b"the same rows").unwrap();
        for _ in 0..4 {
            assert_eq!(seal(&a, KEY_A, b"the same rows").unwrap(), once);
        }
    }

    #[test]
    fn a_different_part_gets_a_different_nonce() {
        let (_d, _g, a, _b) = keys();
        let one = seal(&a, KEY_A, b"rows one").unwrap();
        let two = seal(&a, KEY_A, b"rows two").unwrap();
        assert_ne!(
            &one[MAGIC.len()..MAGIC.len() + NONCE_LEN],
            &two[MAGIC.len()..MAGIC.len() + NONCE_LEN],
            "two different plaintexts must never share a nonce under one key"
        );
    }

    #[test]
    fn another_tenants_key_cannot_open_it() {
        let (_d, _g, a, b) = keys();
        let sealed = seal(&a, KEY_A, b"alice's holdings").unwrap();
        let err = open(&b, KEY_A, &sealed).unwrap_err();
        assert!(matches!(err, ShipError::Ciphertext { .. }), "{err}");
    }

    /// The associated data doing its job: an object that is intact but has
    /// been MOVED does not open.
    #[test]
    fn an_object_moved_to_another_key_does_not_open() {
        let (_d, _g, a, _b) = keys();
        let sealed = seal(&a, KEY_A, b"alice's holdings").unwrap();
        let moved = KEY_A.replace("000000000A", "000000000B");
        assert!(open(&a, &moved, &sealed).is_err());
        // …including a move that only changes the partition date.
        let redated = KEY_A.replace("2026-08-14", "2026-08-13");
        assert!(open(&a, &redated, &sealed).is_err());
    }

    #[test]
    fn a_modified_object_does_not_open() {
        let (_d, _g, a, _b) = keys();
        let mut sealed = seal(&a, KEY_A, b"alice's holdings").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(open(&a, KEY_A, &sealed).is_err());
    }

    #[test]
    fn something_that_is_not_an_envelope_is_refused_by_shape() {
        let (_d, _g, a, _b) = keys();
        assert!(open(&a, KEY_A, b"").is_err());
        assert!(open(&a, KEY_A, b"PAR1 a bare parquet file").is_err());
    }
}
