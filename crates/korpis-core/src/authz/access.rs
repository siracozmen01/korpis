// SPDX-License-Identifier: AGPL-3.0-or-later

//! Access tokens (§9.4 of docs/08-identity.md): what a signed-in browser or CLI presents as the
//! bearer of every API call. One names a user and the session it was exchanged under, for 120
//! seconds, signed by the control plane's token key; it carries no authority, since the authority
//! of each call is evaluated from the grants the user holds when it is made. It is verified by its
//! signature and expiry alone, so ending a session ends its tokens within their lifetime.
//!
//! It is a [`super::sealed`] claim: `korpisat_`, then the payload and its signature, over the
//! purpose label `korpis.access.v1`.

use super::sealed::{self, Claims, Kind};
use super::token::{KeyRing, SigningKey};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub use super::sealed::SealError as AccessError;

/// Every access token starts with this.
pub const ACCESS_PREFIX: &str = "korpisat_";

/// How long an access token is good for.
pub const ACCESS_LIFETIME: Duration = Duration::from_secs(120);

const ACCESS: Kind = Kind { prefix: ACCESS_PREFIX, purpose: b"korpis.access.v1\0" };

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

/// Mints an access token for `claims`, signed by `key`.
#[must_use]
pub fn mint(key: &SigningKey, claims: &AccessClaims) -> String {
    let claims =
        Claims { subject: claims.user, under: claims.session, expires_at: claims.expires_at };
    sealed::seal(key, &ACCESS, &claims)
}

/// What `presented` says, if it is an access token signed by a key in `keys` and not expired at
/// `now`.
pub fn verify(
    keys: &KeyRing,
    presented: &str,
    now: SystemTime,
) -> Result<AccessClaims, AccessError> {
    let c = sealed::open(keys, &ACCESS, presented, now)?;
    Ok(AccessClaims { user: c.subject, session: c.under, expires_at: c.expires_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

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
}
