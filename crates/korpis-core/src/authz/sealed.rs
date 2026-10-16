// SPDX-License-Identifier: AGPL-3.0-or-later

//! Claims the control plane signs with its token key and hands to a client to present back:
//! access tokens ([`super::access`]) and device cookies ([`super::device`]). Each names two ids
//! and an expiry, and each kind has its own prefix and its own purpose label, which the
//! signature covers, so no other thing the key signs can be presented as one of them.
//!
//! The form: the prefix, then the payload and its Ed25519 signature, each base64url without
//! padding, joined by a dot. The payload is a version byte, the key id, the two ids, and the
//! expiry in seconds since the epoch, big-endian.

use super::token::{KeyRing, SigningKey};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, VerifyingKey};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The payload's version.
const VERSION: u8 = 1;
/// The payload's length: version, key id, the two ids, expiry.
const PAYLOAD: usize = 1 + 4 + 16 + 16 + 8;
/// The payload and a 64-byte signature in base64url, and the dot between them.
const ENCODED: usize = 60 + 1 + 86;

/// One kind of sealed claim.
pub(crate) struct Kind {
    /// What a presented one starts with.
    pub prefix: &'static str,
    /// What the signature is over, before the payload.
    pub purpose: &'static [u8],
}

/// What a sealed claim says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Claims {
    /// Whom it names.
    pub subject: Uuid,
    /// What it was issued under.
    pub under: Uuid,
    /// When it stops working.
    pub expires_at: SystemTime,
}

/// Why a presented claim is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SealError {
    /// It is not one of the kind asked for.
    #[error("malformed")]
    Malformed,
    /// It was signed by a key this control plane does not hold, or not signed by it at all.
    #[error("not signed by this control plane")]
    Signature,
    /// It has expired.
    #[error("expired")]
    Expired,
}

/// Seals `claims` as `kind`, signed by `key`.
pub(crate) fn seal(key: &SigningKey, kind: &Kind, claims: &Claims) -> String {
    let seconds = claims.expires_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut payload = Vec::with_capacity(PAYLOAD);
    payload.push(VERSION);
    payload.extend_from_slice(&key.key_id.to_be_bytes());
    payload.extend_from_slice(claims.subject.as_bytes());
    payload.extend_from_slice(claims.under.as_bytes());
    payload.extend_from_slice(&seconds.to_be_bytes());
    let signing = ed25519_dalek::SigningKey::from_bytes(&key.seed());
    let signature = signing.sign(&signed(kind, &payload));
    format!(
        "{}{}.{}",
        kind.prefix,
        URL_SAFE_NO_PAD.encode(&payload),
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

/// What `presented` says, if it is a claim of `kind` signed by a key in `keys` and not expired
/// at `now`.
pub(crate) fn open(
    keys: &KeyRing,
    kind: &Kind,
    presented: &str,
    now: SystemTime,
) -> Result<Claims, SealError> {
    if presented.len() > kind.prefix.len() + ENCODED {
        return Err(SealError::Malformed);
    }
    let body = presented.strip_prefix(kind.prefix).ok_or(SealError::Malformed)?;
    let (payload, signature) = body.split_once('.').ok_or(SealError::Malformed)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| SealError::Malformed)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| SealError::Malformed)?;
    let fields = parse(&payload)?;
    let public = keys.public_bytes(fields.key_id).ok_or(SealError::Signature)?;
    let public = <[u8; 32]>::try_from(public.as_slice()).map_err(|_| SealError::Signature)?;
    let verifying = VerifyingKey::from_bytes(&public).map_err(|_| SealError::Signature)?;
    let signature = Signature::from_slice(&signature).map_err(|_| SealError::Signature)?;
    let message = signed(kind, &payload);
    verifying.verify_strict(&message, &signature).map_err(|_| SealError::Signature)?;
    if now >= fields.claims.expires_at {
        return Err(SealError::Expired);
    }
    Ok(fields.claims)
}

struct Fields {
    key_id: u32,
    claims: Claims,
}

fn parse(payload: &[u8]) -> Result<Fields, SealError> {
    let (&[version], rest) = payload.split_first_chunk::<1>().ok_or(SealError::Malformed)?;
    let (key_id, rest) = rest.split_first_chunk::<4>().ok_or(SealError::Malformed)?;
    let (subject, rest) = rest.split_first_chunk::<16>().ok_or(SealError::Malformed)?;
    let (under, rest) = rest.split_first_chunk::<16>().ok_or(SealError::Malformed)?;
    let (expiry, rest) = rest.split_first_chunk::<8>().ok_or(SealError::Malformed)?;
    if version != VERSION || !rest.is_empty() {
        return Err(SealError::Malformed);
    }
    // Read before the signature is checked, so it is anyone's bytes: an expiry past what a time
    // can hold is malformed, never an overflow.
    let expires_at = UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::from_be_bytes(*expiry)))
        .ok_or(SealError::Malformed)?;
    Ok(Fields {
        key_id: u32::from_be_bytes(*key_id),
        claims: Claims {
            subject: Uuid::from_bytes(*subject),
            under: Uuid::from_bytes(*under),
            expires_at,
        },
    })
}

fn signed(kind: &Kind, payload: &[u8]) -> Vec<u8> {
    let mut message = kind.purpose.to_vec();
    message.extend_from_slice(payload);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: Kind = Kind { prefix: "one_", purpose: b"korpis.test.one\0" };
    const TWO: Kind = Kind { prefix: "one_", purpose: b"korpis.test.two\0" };

    fn ring(key: &SigningKey) -> KeyRing {
        let mut keys = KeyRing::default();
        keys.insert(key.key_id, &key.public_bytes()).unwrap();
        keys
    }

    #[test]
    fn a_claim_opens_only_as_its_own_kind() {
        let key = SigningKey::generate(3);
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let claims = Claims {
            subject: Uuid::now_v7(),
            under: Uuid::now_v7(),
            expires_at: now + Duration::from_secs(60),
        };
        let sealed = seal(&key, &ONE, &claims);
        assert_eq!(open(&ring(&key), &ONE, &sealed, now), Ok(claims));
        assert_eq!(open(&ring(&key), &TWO, &sealed, now), Err(SealError::Signature));
    }

    #[test]
    fn a_forged_payload_is_refused_without_panicking() {
        let key = SigningKey::generate(3);
        let mut payload = vec![VERSION];
        payload.extend_from_slice(&3u32.to_be_bytes());
        payload.extend_from_slice(&[0; 32]);
        payload.extend_from_slice(&u64::MAX.to_be_bytes());
        let forged = format!(
            "{}{}.{}",
            ONE.prefix,
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode([0u8; 64])
        );
        assert_eq!(open(&ring(&key), &ONE, &forged, UNIX_EPOCH), Err(SealError::Malformed));
        let long = format!("{}{}", ONE.prefix, "A".repeat(10_000));
        assert_eq!(open(&ring(&key), &ONE, &long, UNIX_EPOCH), Err(SealError::Malformed));
    }
}
