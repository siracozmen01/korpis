// SPDX-License-Identifier: AGPL-3.0-or-later

//! Re-verifying an identity binding (§5.1 of docs/08-identity.md): its own provider is asked,
//! over its own stream, whether the set is still what it was or the person still known, and the
//! answer moves the binding by the table of §5.1. A provider that cannot be asked fails closed,
//! after the binding's grace. Evaluation reads the binding at every use, so recording the answer
//! is all that suspending or restoring what is rooted in it takes.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{BindingWrite as _, Clock, EffectWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::binding::{BindingState, IdentityBinding, Verification};
use korpis_core::extension::identity::MembersAsked;
use korpis_core::extension::{CallOutcome, CallRequest, ProviderType};
use korpis_core::principal::{Principal, SetName, SystemActor};
use std::time::Duration;
use uuid::Uuid;

/// How long an identity provider has to answer a re-verification.
const VERIFY_CALL: Duration = Duration::from_secs(15);
/// How soon a binding whose provider did not answer is asked again, when its interval is longer:
/// a provider back from an outage restores what it vouches for within a minute.
const RETRY: Duration = Duration::from_secs(60);
/// How long a claim holds a binding against other control planes: its call, and the write after.
const HELD: Duration = Duration::from_secs(30);
/// The most bindings one pass asks about, all at once.
const BATCH: u32 = 32;

/// What a re-verification asks about `b`, and the person it asks about, for a person's binding.
fn asked(b: &IdentityBinding) -> Result<(MembersAsked, Option<&str>), AppError> {
    match &b.subject {
        Principal::ExternalSet { realm, member_of, .. } => {
            let set = SetName { realm: realm.clone(), member_of: member_of.clone() };
            let asked = MembersAsked {
                set: Some(set),
                members: Vec::new(),
                require_membership: b.terms.require_membership.clone(),
            };
            Ok((asked, None))
        }
        Principal::ExternalUser { user, .. } => {
            let asked = MembersAsked {
                set: None,
                members: vec![user.clone()],
                require_membership: Vec::new(),
            };
            Ok((asked, Some(user)))
        }
        _ => Err(AppError::Internal("a binding of no external identity".into())),
    }
}

/// What a re-verification found, and the provider's reason when it found nothing.
fn found(outcome: CallOutcome, person: Option<&str>) -> (Verification, Option<String>) {
    match (outcome, person) {
        (CallOutcome::Members(m), None) if m.set_exists => (Verification::Confirmed, None),
        (CallOutcome::Members(m), Some(p)) if m.present.iter().any(|q| q == p) => {
            (Verification::Confirmed, None)
        }
        (CallOutcome::Members(_), _) => (Verification::Ended, None),
        (CallOutcome::Failed(f), _) => (Verification::Unanswered, Some(f.reason)),
        (_, _) => (Verification::Unanswered, Some("provider.malformed".into())),
    }
}

/// What one re-verification found, and who asked.
struct Answer {
    verification: Verification,
    reason: Option<String>,
    actor: Principal,
    relay: Option<Principal>,
    grant: Option<Uuid>,
}

/// The effect of a binding moving to `state` on `answer`.
fn transition(
    id: Uuid,
    subject: Option<&Principal>,
    state: BindingState,
    answer: &Answer,
) -> Effect {
    let action = match state {
        BindingState::Verified => "identity.binding.verified",
        BindingState::Unverified => "identity.binding.unverified",
    };
    let effect = Effect::new(answer.actor.clone(), action, Outcome::Succeeded);
    let effect = effect.relayed_by(answer.relay.clone()).after(details([
        ("binding", id.to_string().into()),
        ("subject", subject.map(Principal::key).into()),
        ("reason", answer.reason.clone().into()),
    ]));
    match answer.grant {
        Some(g) => effect.under(g),
        None => effect,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Asks `binding`'s provider about it once and records what it found, with an effect if its
    /// state changed, by the person who asked under their grant, or else by the installation's
    /// authority: the binding as it now stands, and the provider's reason when it could not
    /// answer.
    pub(crate) async fn reverify(
        &self,
        binding: IdentityBinding,
        asker: Option<(&Caller, Uuid)>,
    ) -> Result<(IdentityBinding, Option<String>), AppError> {
        let (actor, relay, grant) = match asker {
            Some((c, g)) => {
                (c.principal.clone(), c.relayed.as_ref().map(|r| r.relay.clone()), Some(g))
            }
            None => (Principal::System(SystemActor::Authority), None, None),
        };
        let (request, person) = asked(&binding)?;
        let target = CallTarget {
            organization: binding.provider.organization,
            kind: ProviderType::Identity,
            name: binding.provider.name.clone(),
        };
        let called = self
            .call_own_provider(&target, CallRequest::VerifyMembers(request), VERIFY_CALL)
            .await?;
        let (verification, reason) = found(called.outcome, person);
        let answer = Answer { verification, reason, actor, relay, grant };
        if let Some(recorded) = self.record_answer(binding.clone(), &answer).await? {
            return Ok((recorded, answer.reason));
        }
        // Another answer was recorded since this one was asked. That a subject ended is always
        // safe to say, so it is said whatever was recorded, and asked again within a minute in
        // case it was older; that one is still there may be older than what was recorded, so it
        // is dropped.
        if verification == Verification::Ended {
            self.record_ended(binding.id, &answer).await?;
        }
        let current = self.store.binding(binding.id).await?.filter(|b| b.retired_at.is_none());
        let gone = || AppError::conflict("binding.retired", "the binding was retired");
        Ok((current.ok_or_else(gone)?, answer.reason))
    }

    /// Records that `id`'s subject ended, whatever was recorded meanwhile, with an effect if it
    /// was verified.
    async fn record_ended(&self, id: Uuid, answer: &Answer) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        let before = tx.record_ended(id, self.clock.now() + RETRY).await?;
        if before == Some(BindingState::Verified) {
            tx.append_effect(&transition(id, None, BindingState::Unverified, answer)).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Records `answer` of `binding`, against the verifications it had when it was read, with an
    /// effect if its state changed; `None` if another answer was recorded meanwhile, or it was
    /// retired.
    async fn record_answer(
        &self,
        mut binding: IdentityBinding,
        answer: &Answer,
    ) -> Result<Option<IdentityBinding>, AppError> {
        let now = self.clock.now();
        let asked_at = binding.verifications;
        let changed = binding.verified(answer.verification, now);
        let interval = binding.terms.reverify_interval;
        let due = now + if answer.reason.is_some() { interval.min(RETRY) } else { interval };
        let mut tx = self.store.begin().await?;
        let (state, last) = (binding.state, binding.last_verified_at);
        if !tx.record_verification((binding.id, asked_at), state, last, due).await? {
            return Ok(None);
        }
        binding.verifications = asked_at + 1;
        if changed {
            let effect = transition(binding.id, Some(&binding.subject), state, answer);
            tx.append_effect(&effect).await?;
        }
        tx.commit().await?;
        Ok(Some(binding))
    }

    /// Re-verifies every binding due now, at most a batch of them at once, each claimed so no
    /// other control plane asks the same question meanwhile (§5.1 of docs/08-identity.md); how
    /// many it asked about, and the first failure to record an answer, if any did fail. One
    /// that failed is due again once its claim lapses.
    pub async fn reverify_due(&self) -> Result<(u64, Option<AppError>), AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let due = tx.claim_due_bindings(now, now + HELD, BATCH).await?;
        tx.commit().await?;
        let asked = u64::try_from(due.len()).unwrap_or(u64::MAX);
        let each = due.into_iter().map(|b| self.reverify(b, None));
        let failed = futures_util::future::join_all(each).await.into_iter().find_map(Result::err);
        Ok((asked, failed))
    }

    /// Re-verifies a binding now, under `identity.binding.write` where it is declared, rather
    /// than at its next interval.
    pub async fn verify_identity_binding(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<(IdentityBinding, Option<String>), AppError> {
        let (binding, permit) = self.binding_to_write(caller, id).await?;
        self.reverify(binding, Some((caller, permit.grant))).await
    }
}
