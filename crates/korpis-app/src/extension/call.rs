// SPDX-License-Identifier: AGPL-3.0-or-later

//! Calling a provider (§3.3 and §5 of docs/16-extensions.md). The caller queues the call and
//! waits for it to finish; whichever replica holds a stream of the newest registered version with
//! a slot free claims it and sends it down; the provider's result, the stream's end, or the
//! deadline finishes it. Nothing waits past the deadline, and a call nobody serves fails by it.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, ProviderCallWrite, RecipeSource, Store, Tx, Wakeups};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::{
    CallFailure, CallOutcome, CallRequest, MAX_CALL_DEADLINE, ProviderCall, ProviderType,
    Registration,
};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long a finished call is kept, for whoever reads it after.
const FINISHED_KEPT: Duration = Duration::from_secs(3600);

/// What to call: a provider, by type and name, as an organization sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTarget {
    /// The organization calling; its own provider shadows its ancestors'.
    pub organization: Uuid,
    /// The kind of provider.
    pub kind: ProviderType,
    /// Its name.
    pub name: String,
}

/// A finished call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Called {
    /// The call.
    pub id: Uuid,
    /// The registration it was sent to, and its version, if it was sent.
    pub sent: Option<(Uuid, String)>,
    /// How it ended.
    pub outcome: CallOutcome,
    /// How long it took.
    pub elapsed: Duration,
    /// When it may be asked again, if the provider's breaker refused it.
    pub retry_at: Option<SystemTime>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The organization whose installation serves `target`: the nearest, walking up.
    async fn provider_owner(&self, target: &CallTarget) -> Result<Option<Uuid>, AppError> {
        let chain = self.organization_chain(target.organization).await?;
        let providing =
            self.store.providing_organizations(&chain, target.kind, &target.name).await?;
        Ok(chain.into_iter().find(|o| providing.contains(o)))
    }

    /// Calls a provider and waits, at most `timeout` and never past [`MAX_CALL_DEADLINE`], for
    /// the call to finish. Internal: the use case asking decides who may.
    pub async fn call_provider(
        &self,
        target: &CallTarget,
        request: CallRequest,
        timeout: Duration,
    ) -> Result<Called, AppError> {
        let started = self.clock.now();
        let Some(organization) = self.provider_owner(target).await? else {
            let outcome = CallOutcome::Failed(CallFailure::unknown());
            let elapsed = Duration::ZERO;
            return Ok(Called { id: Uuid::now_v7(), sent: None, outcome, elapsed, retry_at: None });
        };
        let call = ProviderCall {
            id: Uuid::now_v7(),
            organization,
            kind: target.kind,
            name: target.name.clone(),
            request,
            deadline: started + timeout.min(MAX_CALL_DEADLINE),
            created_at: started,
            sent: None,
            finished: None,
        };
        let mut wakeups = self.store.provider_calls();
        if let Some(until) = self.admit_call(&call).await? {
            let outcome = CallOutcome::Failed(CallFailure::circuit_open());
            let elapsed = Duration::ZERO;
            return Ok(Called { id: call.id, sent: None, outcome, elapsed, retry_at: Some(until) });
        }
        let (sent, outcome, at) = self.finished(&call, &mut wakeups).await?;
        self.judge_provider(&call, &outcome, at).await?;
        let elapsed = at.duration_since(started).unwrap_or_default();
        Ok(Called { id: call.id, sent, outcome, elapsed, retry_at: None })
    }

    /// Waits for `call` to finish, finishing it at its deadline if nothing else has: where it
    /// was sent, how it ended, and when.
    async fn finished(
        &self,
        call: &ProviderCall,
        wakeups: &mut S::Wakeups,
    ) -> Result<(Option<(Uuid, String)>, CallOutcome, SystemTime), AppError> {
        loop {
            let now = self.clock.now();
            let read = self.store.provider_call(call.id).await?;
            let read = read.ok_or_else(|| AppError::Internal("a queued call vanished".into()))?;
            if let Some((outcome, at)) = read.finished {
                return Ok((read.sent, outcome, at));
            }
            if now >= call.deadline && self.finish_late(call.id, now).await? {
                return Ok((read.sent, CallOutcome::Failed(CallFailure::deadline()), now));
            }
            // A wake-up or the deadline, whichever is first; then re-read.
            wakeups.next_before(call.deadline).await;
        }
    }

    /// Finishes a call whose deadline passed, unless something finished it first; whether it did.
    async fn finish_late(&self, call: Uuid, now: SystemTime) -> Result<bool, AppError> {
        let late = CallOutcome::Failed(CallFailure::deadline());
        let mut tx = self.store.begin().await?;
        let won = tx.finish_call(call, None, &late, now).await?;
        tx.commit().await?;
        Ok(won)
    }

    /// Pings a provider as `organization` sees it, under `extension.read` over it.
    pub async fn ping_provider(
        &self,
        caller: &Caller,
        target: &CallTarget,
        timeout: Duration,
    ) -> Result<Called, AppError> {
        let object = ObjectRef::new(Kind::Organization, target.organization);
        self.authorize(caller, "extension.read", object).await?;
        let nonce = Uuid::now_v7().simple().to_string();
        self.call_provider(target, CallRequest::Ping { nonce }, timeout).await
    }

    /// The registrations serving in an organization, under `extension.read` over it.
    pub async fn list_providers(
        &self,
        caller: &Caller,
        organization: Uuid,
    ) -> Result<Vec<Registration>, AppError> {
        let object = ObjectRef::new(Kind::Organization, organization);
        self.authorize(caller, "extension.read", object).await?;
        Ok(self.store.registrations_in(organization).await?)
    }

    /// Wake-ups for a dispatcher, from now on: a call was queued or finished.
    pub fn provider_call_wakeups(&self) -> S::Wakeups {
        self.store.provider_calls()
    }

    /// Claims for `registration`, a stream this replica holds, the calls it may take now.
    pub async fn claim_provider_calls(
        &self,
        registration: &Registration,
    ) -> Result<Vec<ProviderCall>, AppError> {
        let mut tx = self.store.begin().await?;
        let claimed = tx.claim_calls(registration, self.clock.now()).await?;
        tx.commit().await?;
        Ok(claimed)
    }

    /// Records what `registration`'s provider answered `call`; `false` if the call had already
    /// finished (its deadline passed) or was never sent to it.
    pub async fn finish_provider_call(
        &self,
        registration: Uuid,
        call: Uuid,
        outcome: &CallOutcome,
    ) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let done = tx.finish_call(call, Some(registration), outcome, self.clock.now()).await?;
        tx.commit().await?;
        Ok(done)
    }

    /// Fails the calls nobody can answer any more (sent to a stream that is gone, or queued for
    /// a provider uninstalled meanwhile) and forgets calls finished long ago; how many it failed.
    pub async fn sweep_provider_calls(&self) -> Result<u64, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let ended = CallOutcome::Failed(CallFailure::stream_ended());
        let orphaned = tx.fail_calls_without_registration(&ended, now).await?;
        let unknown = CallOutcome::Failed(CallFailure::unknown());
        let unserved = tx.fail_calls_without_provider(&unknown, now).await?;
        tx.prune_calls(now.checked_sub(FINISHED_KEPT).unwrap_or(SystemTime::UNIX_EPOCH)).await?;
        tx.commit().await?;
        Ok(orphaned + unserved)
    }
}
