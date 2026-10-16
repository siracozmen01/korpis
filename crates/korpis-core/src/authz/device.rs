// SPDX-License-Identifier: AGPL-3.0-or-later

//! Device cookies (§9.6 of docs/08-identity.md), after OWASP's: a browser that has signed in to
//! an account holds one, naming the account and a device id of its own, signed by the control
//! plane's token key, and the guesses at the account's password from that browser are counted
//! against the device alone rather than against the sign-in name, so a stranger spending the
//! name's guesses cannot keep the holder out of a browser they have used. It carries no
//! authority: it only says which bucket a sign-in's guesses are charged to.
//!
//! It is a [`super::sealed`] claim: `korpisdc_`, then the payload and its signature, over the
//! purpose label `korpis.device.v1`.

use super::sealed::{self, Claims, Kind};
use super::token::{KeyRing, SigningKey};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub use super::sealed::SealError as DeviceError;

/// Every device cookie starts with this.
pub const DEVICE_PREFIX: &str = "korpisdc_";

/// How long a device cookie is honoured after the sign-in that gave it; each sign-in from the
/// browser gives it a new one.
pub const DEVICE_LIFETIME: Duration = Duration::from_secs(180 * 24 * 60 * 60);

const DEVICE: Kind = Kind { prefix: DEVICE_PREFIX, purpose: b"korpis.device.v1\0" };

/// What a device cookie says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceClaims {
    /// The account the browser signed in to.
    pub account: Uuid,
    /// The browser, as the cookie names it: its guesses are counted under this.
    pub device: Uuid,
    /// When it stops being honoured.
    pub expires_at: SystemTime,
}

impl DeviceClaims {
    /// When it was issued: its lifetime before it expires, to the second, since the expiry it
    /// carries is kept in whole seconds.
    #[must_use]
    pub fn issued_at(&self) -> SystemTime {
        self.expires_at.checked_sub(DEVICE_LIFETIME).unwrap_or(SystemTime::UNIX_EPOCH)
    }
}

/// Mints a device cookie's value for `claims`, signed by `key`.
#[must_use]
pub fn mint(key: &SigningKey, claims: &DeviceClaims) -> String {
    let claims =
        Claims { subject: claims.account, under: claims.device, expires_at: claims.expires_at };
    sealed::seal(key, &DEVICE, &claims)
}

/// What `presented` says, if it is a device cookie signed by a key in `keys` and not expired at
/// `now`.
pub fn verify(
    keys: &KeyRing,
    presented: &str,
    now: SystemTime,
) -> Result<DeviceClaims, DeviceError> {
    let c = sealed::open(keys, &DEVICE, presented, now)?;
    Ok(DeviceClaims { account: c.subject, device: c.under, expires_at: c.expires_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::access::{self, AccessClaims};
    use std::time::UNIX_EPOCH;

    #[test]
    fn a_device_cookie_is_not_an_access_token_nor_one_a_device_cookie() {
        let key = SigningKey::generate(3);
        let mut keys = KeyRing::default();
        keys.insert(key.key_id, &key.public_bytes()).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let (account, device) = (Uuid::now_v7(), Uuid::now_v7());
        let claims = DeviceClaims { account, device, expires_at: now + DEVICE_LIFETIME };
        let cookie = mint(&key, &claims);
        assert_eq!(verify(&keys, &cookie, now), Ok(claims));
        assert_eq!(verify(&keys, &cookie, now + DEVICE_LIFETIME), Err(DeviceError::Expired));
        let as_access = cookie.replacen(DEVICE_PREFIX, access::ACCESS_PREFIX, 1);
        assert_eq!(access::verify(&keys, &as_access, now), Err(DeviceError::Signature));
        let token = access::mint(
            &key,
            &AccessClaims { user: account, session: device, expires_at: now + DEVICE_LIFETIME },
        );
        let as_device = token.replacen(access::ACCESS_PREFIX, DEVICE_PREFIX, 1);
        assert_eq!(verify(&keys, &as_device, now), Err(DeviceError::Signature));
    }
}
