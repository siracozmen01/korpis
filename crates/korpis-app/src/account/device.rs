// SPDX-License-Identifier: AGPL-3.0-or-later

//! Device cookies (§9.6 of docs/08-identity.md), after OWASP's: which bucket a guess at a
//! password is charged to. A browser that has signed in to the account it names is charged to
//! its own bucket, and to one the account's devices share, which no browser without a cookie can
//! reach; any other is charged to the sign-in name's. A stranger spending the name's guesses
//! therefore keeps its holder out of new browsers only, and once a device has spent its own it
//! is no longer honoured and is charged as a browser without one.

use super::own_account;
use super::sign_in::refused;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::account::record::sign_in_name;
use korpis_core::authz::device::{self, DEVICE_LIFETIME, DeviceClaims};
use korpis_core::rate::Bucket;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The most device cookies one sign-in is asked about: a browser signed in to more accounts
/// than this is charged as one without a cookie for the rest.
const MOST_PRESENTED: usize = 16;

/// A device cookie given to a browser that has just signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The account it names, which its name carries, so a browser holds one per account.
    pub account: Uuid,
    /// Its value.
    pub cookie: String,
    /// When it stops being honoured.
    pub expires_at: SystemTime,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A fresh device cookie for the browser the caller is calling from, which a change to the
    /// account's secrets has just stopped honouring the one it held.
    pub fn device_cookie(&self, caller: &Caller) -> Result<Device, AppError> {
        Ok(self.device_for(own_account(caller)?))
    }

    /// A device cookie for a browser that has just signed in to `account`.
    pub(super) fn device_for(&self, account: Uuid) -> Device {
        let expires_at = self.clock.now() + DEVICE_LIFETIME;
        let claims = DeviceClaims { account, device: Uuid::now_v7(), expires_at };
        Device { account, cookie: device::mint(&self.token_key, &claims), expires_at }
    }

    /// Charges a guess at `name`'s password from `ip`, holding `presented` device cookies, to
    /// its buckets, all or none, and answers them, to be given back if it is right. Refused, it
    /// is [`AppError::RateLimited`] and nothing is checked.
    pub(super) async fn charge_guess(
        &self,
        ip: Option<IpAddr>,
        name: &str,
        presented: &[String],
    ) -> Result<Vec<Bucket>, AppError> {
        let source = self.failure_bucket(ip);
        let now = self.clock.now();
        if let Some(trusted) = self.trusted(name, presented).await? {
            let buckets = [vec![source.clone()], self.device_buckets(trusted)].concat();
            let standing = self.store.charge(buckets.clone(), now).await?;
            if standing.iter().all(|s| s.admitted) {
                return Ok(buckets);
            }
            if !standing.first().is_some_and(|s| s.admitted) {
                refused(standing)?;
            }
        }
        let rate = self.limiter.rates.guesses;
        let named = sign_in_name(name).ok().map(|n| Bucket {
            policy: "guesses",
            key: format!("n:{n}"),
            rate,
        });
        let buckets: Vec<Bucket> = std::iter::once(source).chain(named).collect();
        refused(self.store.charge(buckets.clone(), now).await?)?;
        Ok(buckets)
    }

    /// The buckets a device's guesses are charged to: its own, and the one its account's
    /// devices share, so collecting cookies by signing in again and again buys no more guesses.
    fn device_buckets(&self, trusted: DeviceClaims) -> Vec<Bucket> {
        let rates = &self.limiter.rates;
        vec![
            Bucket {
                policy: "device-guesses",
                key: format!("d:{}", trusted.device.simple()),
                rate: rates.device_guesses,
            },
            Bucket {
                policy: "account-guesses",
                key: format!("a:{}", trusted.account.simple()),
                rate: rates.account_guesses,
            },
        ]
    }

    /// The device cookie among `presented` that names the account `name` names, if one does
    /// and was issued no earlier than the second its password or TOTP last changed: a browser
    /// that signed in with a secret since replaced is a browser without a cookie (Finding 73 of
    /// docs/23-walkthroughs.md).
    /// Only a cookie this control plane signed is looked at further, and only then is the name
    /// looked up, so a browser without one learns nothing from how long this takes.
    async fn trusted(
        &self,
        name: &str,
        presented: &[String],
    ) -> Result<Option<DeviceClaims>, AppError> {
        let (keys, now) = (self.token_ring()?, self.clock.now());
        let held: Vec<DeviceClaims> = presented
            .iter()
            .take(MOST_PRESENTED)
            .filter_map(|p| device::verify(&keys, p, now).ok())
            .collect();
        if held.is_empty() {
            return Ok(None);
        }
        let Ok(name) = sign_in_name(name) else { return Ok(None) };
        let Some(account) = self.store.account_named(&name).await? else { return Ok(None) };
        let Some(found) = held.into_iter().find(|c| c.account == account.id) else {
            return Ok(None);
        };
        let changed = self.store.secrets_changed_at(account.id).await?;
        Ok(changed
            .is_none_or(|changed| seconds(found.issued_at()) >= seconds(changed))
            .then_some(found))
    }
}

/// Whole seconds since the epoch: a device cookie's issue is known to the second.
fn seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
