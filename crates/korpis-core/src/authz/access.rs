// SPDX-License-Identifier: AGPL-3.0-or-later

//! Access tokens (§9.4 of docs/08-identity.md): what a signed-in browser or CLI presents as the
//! bearer of every API call. One names a user and the session it was exchanged under, for 120
//! seconds, signed by the control plane's token key; it carries no authority, since the authority
//! of each call is evaluated from the grants the user holds when it is made. It is verified by its
//! signature and expiry alone, so ending a session ends its tokens within their lifetime.
//!
//! The form: `korpisat_`, then the payload and its Ed25519 signature, each base64url without
//! padding, joined by a dot. The payload is a version byte, the key id, the user, the session,
//! and the expiry in seconds since the epoch, big-endian; the signature is over a purpose label
//! and the payload, so no other thing the key signs can be presented as one.

use super::token::{KeyRing, SigningKey};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer as _, VerifyingKey};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Every access token starts with this.
pub const ACCESS_PREFIX: &str = "korpisat_";

/// How long an access token is good for.
pub const ACCESS_LIFETIME: Duration = Duration::from_secs(120);

/// What the signature is over, before the payload.
const PURPOSE: &[u8] = b"korpis.access.v1\0";
/// The payload's version.
const VERSION: u8 = 1;
/// The payload's length: version, key id, user, session, expiry.
const PAYLOAD: usize = 1 + 4 + 16 + 16 + 8;
/// The longest a presented token can be: the prefix, the payload and a 64-byte signature in
/// base64url, and the dot. Anything longer is refused before it is decoded.
const LONGEST: usize = ACCESS_PREFIX.len() + 60 + 1 + 86;

/// What an access token says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessClaims {
    /// The user it was issued to.
    pub user: Uuid,
    /// The session it was exchanged under.
    pub session: Uuid,
    /// When it stops working.
    pub expires_at: SystemTime,
}

/// Why a presented access token is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AccessError {
    /// It is not an access token.
    #[error("not an access token")]
    Malformed,
    /// It was signed by a key this control plane does not hold, or not signed by it at all.
    #[error("not signed by this control plane")]
    Signature,
    /// It has expired.
    #[error("expired")]
    Expired,
}

/// Mints an access token for `claims`, signed by `key`.
#[must_use]
pub fn mint(key: &SigningKey, claims: &AccessClaims) -> String {
    let seconds = claims.expires_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut payload = Vec::with_capacity(PAYLOAD);
    payload.push(VERSION);
    payload.extend_from_slice(&key.key_id.to_be_bytes());
    payload.extend_from_slice(claims.user.as_bytes());
    payload.extend_from_slice(claims.session.as_bytes());
    payload.extend_from_slice(&seconds.to_be_bytes());
    let signing = ed25519_dalek::SigningKey::from_bytes(&key.seed());
    let signature = signing.sign(&signed(&payload));
    format!(
        "{ACCESS_PREFIX}{}.{}",
        URL_SAFE_NO_PAD.encode(&payload),
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

/// What `presented` says, if it is an access token signed by a key in `keys` and not expired at
/// `now`.
pub fn verify(
    keys: &KeyRing,
    presented: &str,
    now: SystemTime,
) -> Result<AccessClaims, AccessError> {
    if presented.len() > LONGEST {
        return Err(AccessError::Malformed);
    }
    let body = presented.strip_prefix(ACCESS_PREFIX).ok_or(AccessError::Malformed)?;
    let (payload, signature) = body.split_once('.').ok_or(AccessError::Malformed)?;
    let payload = URL_SAFE_NO_PAD.decode(payload).map_err(|_| AccessError::Malformed)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| AccessError::Malformed)?;
    let fields = parse(&payload)?;
    let public = keys.public_bytes(fields.key_id).ok_or(AccessError::Signature)?;
    let public = <[u8; 32]>::try_from(public.as_slice()).map_err(|_| AccessError::Signature)?;
    let verifying = VerifyingKey::from_bytes(&public).map_err(|_| AccessError::Signature)?;
    let signature = Signature::from_slice(&signature).map_err(|_| AccessError::Signature)?;
    verifying.verify_strict(&signed(&payload), &signature).map_err(|_| AccessError::Signature)?;
    if now >= fields.claims.expires_at {
        return Err(AccessError::Expired);
    }
    Ok(fields.claims)
}

struct Fields {
    key_id: u32,
    claims: AccessClaims,
}

fn parse(payload: &[u8]) -> Result<Fields, AccessError> {
    let (&[version], rest) = payload.split_first_chunk::<1>().ok_or(AccessError::Malformed)?;
    let (key_id, rest) = rest.split_first_chunk::<4>().ok_or(AccessError::Malformed)?;
    let (user, rest) = rest.split_first_chunk::<16>().ok_or(AccessError::Malformed)?;
    let (session, rest) = rest.split_first_chunk::<16>().ok_or(AccessError::Malformed)?;
    let (expiry, rest) = rest.split_first_chunk::<8>().ok_or(AccessError::Malformed)?;
    if version != VERSION || !rest.is_empty() {
        return Err(AccessError::Malformed);
    }
    // Read before the signature is checked, so it is anyone's bytes: an expiry past what a time
    // can hold is malformed, never an overflow.
    let expires_at = UNIX_EPOCH
        .checked_add(Duration::from_secs(u64::from_be_bytes(*expiry)))
        .ok_or(AccessError::Malformed)?;
    Ok(Fields {
        key_id: u32::from_be_bytes(*key_id),
        claims: AccessClaims {
            user: Uuid::from_bytes(*user),
            session: Uuid::from_bytes(*session),
            expires_at,
        },
    })
}

fn signed(payload: &[u8]) -> Vec<u8> {
    let mut message = PURPOSE.to_vec();
    message.extend_from_slice(payload);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(key: &SigningKey) -> KeyRing {
        let mut keys = KeyRing::default();
        keys.insert(key.key_id, &key.public_bytes()).unwrap();
        keys
    }

    #[test]
    fn an_access_token_verifies_until_it_expires_and_only_under_its_key() {
        let key = SigningKey::generate(3);
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let claims = AccessClaims {
            user: Uuid::now_v7(),
            session: Uuid::now_v7(),
            expires_at: now + ACCESS_LIFETIME,
        };
        let token = mint(&key, &claims);
        assert!(token.starts_with(ACCESS_PREFIX));
        assert_eq!(verify(&ring(&key), &token, now), Ok(claims));
        assert_eq!(verify(&ring(&key), &token, now + ACCESS_LIFETIME), Err(AccessError::Expired));
        let other = SigningKey::generate(3);
        assert_eq!(verify(&ring(&other), &token, now), Err(AccessError::Signature));
        assert_eq!(verify(&KeyRing::default(), &token, now), Err(AccessError::Signature));
        let mut forged = token.clone();
        forged.replace_range(12..13, if &token[12..13] == "A" { "B" } else { "A" });
        assert!(verify(&ring(&key), &forged, now).is_err(), "a payload altered");
        assert_eq!(verify(&ring(&key), "korpis_x", now), Err(AccessError::Malformed));
    }

    #[test]
    fn a_forged_payload_is_refused_without_panicking() {
        let key = SigningKey::generate(3);
        let mut payload = vec![VERSION];
        payload.extend_from_slice(&3u32.to_be_bytes());
        payload.extend_from_slice(&[0; 32]);
        payload.extend_from_slice(&u64::MAX.to_be_bytes());
        let forged = format!(
            "{ACCESS_PREFIX}{}.{}",
            URL_SAFE_NO_PAD.encode(&payload),
            URL_SAFE_NO_PAD.encode([0u8; 64])
        );
        assert_eq!(verify(&ring(&key), &forged, UNIX_EPOCH), Err(AccessError::Malformed));
        let long = format!("{ACCESS_PREFIX}{}", "A".repeat(10_000));
        assert_eq!(verify(&ring(&key), &long, UNIX_EPOCH), Err(AccessError::Malformed));
    }
}
