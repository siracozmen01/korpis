// SPDX-License-Identifier: AGPL-3.0-or-later

//! A provider's circuit breaker at work (§5 of docs/16-extensions.md). A call is admitted, or
//! refused at once, by the breaker of the provider it calls, in the transaction that queues it;
//! when it has finished, its caller judges the provider by the calls it last finished, and trips
//! the breaker, or closes it after a probe, in a transaction holding the breaker's lock, so two
//! replicas never both decide. Each decision that changes the provider's condition is an event.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{BreakerWrite, Clock, ProviderCallWrite, ProviderKey, RecipeSource, Store, Tx};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::{
    Admission, Breaker, CallOutcome, Measured, ProviderCall, ProviderChange, ProviderCondition,
    ProviderType, Sample, WINDOW_AGE, judge, measure,
};
use std::time::SystemTime;
use uuid::Uuid;

/// A provider's health as published: its breaker and what was last measured of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderHealth {
    /// Its type.
    pub kind: ProviderType,
    /// Its name.
    pub name: String,
    /// Its breaker.
    pub breaker: Breaker,
    /// Its recent calls.
    pub measured: Measured,
}

fn key(call: &ProviderCall) -> ProviderKey<'_> {
    ProviderKey { organization: call.organization, kind: call.kind, name: &call.name }
}

/// The breaker's view of one finished call.
fn sample(call: &ProviderCall, outcome: &CallOutcome, at: SystemTime) -> Sample {
    let span = |to: SystemTime| to.duration_since(call.created_at).unwrap_or_default();
    Sample {
        failed: matches!(outcome, CallOutcome::Failed(f) if f.unwell()),
        elapsed: span(at),
        allowed: span(call.deadline),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Queues `call` if its provider's breaker admits it; when it is refused, when it may be
    /// asked again.
    pub(super) async fn admit_call(
        &self,
        call: &ProviderCall,
    ) -> Result<Option<SystemTime>, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let refused =
            match tx.lock_breaker(key(call), now).await?.admit(call.id, call.deadline, now) {
                Admission::Refuse { until } => Some(until),
                Admission::Probe(probing) => {
                    tx.set_breaker(key(call), &probing, now).await?;
                    None
                }
                Admission::Admit => None,
            };
        if refused.is_none() {
            tx.insert_call(call).await?;
        }
        // Ended explicitly either way: the lock gave the transaction an identifier, and one left
        // to roll back whenever its connection is next used holds back the event log's horizon.
        tx.commit().await?;
        Ok(refused)
    }

    /// Judges `call`'s provider now that `call` finished with `outcome` at `at`.
    pub(super) async fn judge_provider(
        &self,
        call: &ProviderCall,
        outcome: &CallOutcome,
        at: SystemTime,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let breaker = tx.lock_breaker(key(call), now).await?;
        let change = match breaker {
            Breaker::HalfOpen { probe, .. } if probe == call.id => {
                let next = breaker.probed(&sample(call, outcome, at), now);
                tx.set_breaker(key(call), &next, now).await?;
                (next == Breaker::Closed).then_some(ProviderChange::Recovered)
            }
            Breaker::Closed => {
                let since = now.checked_sub(WINDOW_AGE).unwrap_or(SystemTime::UNIX_EPOCH);
                let measured = measure(&tx.recent_samples(key(call), since).await?);
                match judge(&measured) {
                    Some(trip) => {
                        tx.set_breaker(key(call), &Breaker::tripped(now), now).await?;
                        let queued = tx.queued_calls(key(call)).await?;
                        Some(ProviderChange::Degraded { trip, measured, queued })
                    }
                    None => None,
                }
            }
            Breaker::Open { .. } | Breaker::HalfOpen { .. } => None,
        };
        if let Some(change) = change {
            let condition = ProviderCondition {
                organization: call.organization,
                kind: call.kind,
                name: call.name.clone(),
                at: now,
                change,
            };
            tx.append_provider_condition(&condition).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The health of every provider an organization's installations serve, under
    /// `extension.read` over it.
    pub async fn provider_health(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<Vec<ProviderHealth>, AppError> {
        let object = ObjectRef::new(Kind::Organization, organization);
        self.authorize(caller, "extension.read", object).await?;
        let provides = self.store.provides_in(organization).await?;
        let since = self.clock.now().checked_sub(WINDOW_AGE).unwrap_or(SystemTime::UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        let mut health = Vec::with_capacity(provides.len());
        for p in provides {
            let k = ProviderKey { organization, kind: p.kind, name: &p.name };
            let breaker = tx.read_breaker(k).await?;
            let measured = measure(&tx.recent_samples(k, since).await?);
            health.push(ProviderHealth { kind: p.kind, name: p.name, breaker, measured });
        }
        Ok(health)
    }
}
