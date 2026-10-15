// SPDX-License-Identifier: AGPL-3.0-or-later

//! The credentials the provider mints, one per grant: an access key, its secret, and a session
//! token, each random, scoped to one prefix and one mode, and gone when they expire. They live in
//! memory alone, so a provider that restarts has issued none, and none outlives the access the
//! control plane recorded (§5.5 of docs/06-storage.md).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// What a grant allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Put, get, and list.
    Write,
    /// Get and list.
    Read,
}

/// One grant.
#[derive(Clone, PartialEq, Eq)]
pub struct Grant {
    /// The access key.
    pub access_key_id: String,
    /// Its secret.
    pub secret: String,
    /// The session token.
    pub session_token: String,
    /// The prefix every key it reaches begins with.
    pub prefix: String,
    /// What it allows.
    pub mode: Mode,
    /// When it ends.
    pub expires: SystemTime,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("access_key_id", &self.access_key_id)
            .field("prefix", &self.prefix)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

/// `n` random bytes, in hex.
fn random(n: usize) -> Result<String, getrandom::Error> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)?;
    Ok(crate::hex(&bytes))
}

/// The grants in force.
#[derive(Debug, Default)]
pub struct Tokens {
    grants: Mutex<HashMap<String, Grant>>,
}

impl Tokens {
    /// A grant of `mode` to `prefix` for `ttl` from `now`.
    ///
    /// # Errors
    /// The system has no randomness to give.
    pub fn mint(
        &self,
        prefix: &str,
        mode: Mode,
        (now, ttl): (SystemTime, Duration),
    ) -> Result<Grant, getrandom::Error> {
        let grant = Grant {
            access_key_id: format!("KF{}", random(10)?.to_ascii_uppercase()),
            secret: random(32)?,
            session_token: random(32)?,
            prefix: prefix.to_owned(),
            mode,
            expires: now + ttl,
        };
        let mut grants = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        grants.retain(|_, g| g.expires > now);
        grants.insert(grant.access_key_id.clone(), grant.clone());
        Ok(grant)
    }

    /// The grant of `access_key_id`, if it is in force at `now`.
    #[must_use]
    pub fn get(&self, access_key_id: &str, now: SystemTime) -> Option<Grant> {
        let grants = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
        grants.get(access_key_id).filter(|g| g.expires > now).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_is_found_until_it_expires_and_each_is_its_own() {
        let t = Tokens::default();
        let now = SystemTime::now();
        let a = t.mint("w/1/", Mode::Write, (now, Duration::from_secs(60))).unwrap();
        let b = t.mint("w/2/", Mode::Read, (now, Duration::from_secs(60))).unwrap();
        assert_ne!((&a.access_key_id, &a.secret), (&b.access_key_id, &b.secret));
        assert_eq!(t.get(&a.access_key_id, now), Some(a.clone()));
        assert_eq!(t.get(&a.access_key_id, now + Duration::from_secs(61)), None);
        assert_eq!(t.get("KFNONE", now), None);
        assert!(!format!("{a:?}").contains(&a.secret), "never printed");
    }
}
