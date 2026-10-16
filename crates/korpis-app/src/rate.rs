// SPDX-License-Identifier: AGPL-3.0-or-later

//! Charging a request to its buckets (§10 of docs/10-api.md): the subject's own, and each
//! organization it holds a grant in, the organization of the grant's scope, which attenuation
//! never leaves; an expensive request to a second pair beside them. The organizations a subject
//! holds grants in are read once a minute, not on every request.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::principal::Principal;
use korpis_core::rate::{Bucket, Rates, Standing};
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long the organizations a subject holds grants in are taken as read.
const ORGANIZATIONS_KEPT: Duration = Duration::from_secs(60);
/// How many subjects' organizations are kept before they are all read again.
const ORGANIZATIONS_HELD: usize = 10_000;

/// The rates an installation charges, and what it remembers to charge them.
#[derive(Debug, Default)]
pub struct Limiter {
    pub(crate) rates: Rates,
    organizations: Mutex<HashMap<String, (SystemTime, Vec<Uuid>)>>,
}

impl Limiter {
    /// Charging at `rates`.
    #[must_use]
    pub fn new(rates: Rates) -> Self {
        Self { rates, organizations: Mutex::default() }
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organizations `subject` holds a live grant in: the organization of each one's scope. A
    /// grant revoked, suspended, or expired, or descending from one that is, charges nothing, so
    /// an organization that revokes a subject stops paying for it within a minute.
    async fn organizations_of(&self, subject: &Principal) -> Result<Vec<Uuid>, AppError> {
        let key = subject.key();
        let now = self.clock.now();
        {
            let held = self.limiter.organizations.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((at, orgs)) = held.get(&key)
                && now.duration_since(*at).unwrap_or_default() < ORGANIZATIONS_KEPT
            {
                return Ok(orgs.clone());
            }
        }
        let orgs = self.store.organizations_held(&key, now).await?;
        let mut held = self.limiter.organizations.lock().unwrap_or_else(PoisonError::into_inner);
        if held.len() >= ORGANIZATIONS_HELD {
            held.clear();
        }
        held.insert(key, (now, orgs.clone()));
        Ok(orgs)
    }

    /// Charges a request that presents no credential, from `ip`, to its source's bucket at the
    /// individual rate (§10 of docs/10-api.md): the sign-in and redemption ceremonies, which
    /// anyone may begin, each writing a challenge.
    pub async fn charge_source(
        &self,
        ip: Option<std::net::IpAddr>,
    ) -> Result<Vec<Standing>, AppError> {
        let source = korpis_core::audit::failure::source(ip);
        let bucket = Bucket {
            policy: "source",
            key: format!("s:{source}"),
            rate: self.limiter.rates.individual,
        };
        let standing = self.store.charge(vec![bucket], self.clock.now()).await?;
        let refused = standing.iter().filter(|s| !s.admitted).map(Standing::retry_after).max();
        match refused {
            Some(retry_after) => Err(AppError::RateLimited { retry_after, standing }),
            None => Ok(standing),
        }
    }

    /// Charges `caller`'s request, `expensive` or not, to its buckets; where each stands, or
    /// `RateLimited` if any refused it, which then charged none.
    pub async fn charge_request(
        &self,
        caller: &Caller,
        expensive: bool,
    ) -> Result<Vec<Standing>, AppError> {
        if matches!(caller.principal, Principal::System(_)) {
            return Ok(Vec::new());
        }
        let rates = self.limiter.rates;
        let subject = caller.principal.key();
        let organizations = self.organizations_of(&caller.principal).await?;
        let mut buckets = vec![Bucket {
            policy: "individual",
            key: format!("i:{subject}"),
            rate: rates.individual,
        }];
        for o in &organizations {
            let key = format!("o:{o}");
            buckets.push(Bucket { policy: "organization", key, rate: rates.organization });
        }
        if expensive {
            let rate = rates.expensive_individual;
            buckets.push(Bucket {
                policy: "expensive-individual",
                key: format!("x:{subject}"),
                rate,
            });
            for o in &organizations {
                let rate = rates.expensive_organization;
                let key = format!("xo:{o}");
                buckets.push(Bucket { policy: "expensive-organization", key, rate });
            }
        }
        let standing = self.store.charge(buckets, self.clock.now()).await?;
        let refused = standing.iter().filter(|s| !s.admitted).map(Standing::retry_after).max();
        match refused {
            Some(retry_after) => Err(AppError::RateLimited { retry_after, standing }),
            None => Ok(standing),
        }
    }
}
