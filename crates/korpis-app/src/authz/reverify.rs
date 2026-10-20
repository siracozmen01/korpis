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
use crate::ports::{
    BindingWrite as _, Clock, EffectWrite as _, MemberWrite as _, RecipeSource, Store, Tx as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::binding::{BindingState, IdentityBinding, Verification};
use korpis_core::authz::membership::MemberRecord;
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
fn asked<'b>(
    b: &'b IdentityBinding,
    delegated: &[MemberRecord],
) -> Result<(MembersAsked, Option<&'b str>), AppError> {
    match &b.subject {
        Principal::ExternalSet { realm, member_of, .. } => {
            let set = SetName { realm: realm.clone(), member_of: member_of.clone() };
            let asked = MembersAsked {
                set: Some(set),
                members: delegated.iter().filter_map(|m| user_of(&m.member)).collect(),
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

/// A member's identifier at their provider.
fn user_of(member: &Principal) -> Option<String> {
    match member {
        Principal::ExternalUser { user, .. } => Some(user.clone()),
        _ => None,
    }
}

/// Of the members asked about, the keys of those an answer says are in the set and of those it
/// says are not; nothing when it did not answer. A member it says it cannot tell about is in
/// neither, and keeps their last confirmation (§5.1); a set that no longer exists holds no one.
fn members_found(outcome: &CallOutcome, delegated: &[MemberRecord]) -> (Vec<String>, Vec<String>) {
    let CallOutcome::Members(m) = outcome else { return (Vec::new(), Vec::new()) };
    let (mut present, mut absent) = (Vec::new(), Vec::new());
    for d in delegated {
        let Some(user) = user_of(&d.member) else { continue };
        if m.set_exists && m.present.contains(&user) {
            present.push(d.member.key());
        } else if !m.set_exists || !m.unknown.contains(&user) {
            absent.push(d.member.key());
        }
    }
    (present, absent)
}

/// What a re-verification found, and the provider's reason when it found nothing.
fn found(outcome: CallOutcome, person: Option<&str>) -> (Verification, Option<String>) {
    match (outcome, person) {
        (CallOutcome::Members(m), None) if m.set_exists => (Verification::Confirmed, None),
        (CallOutcome::Members(m), Some(p)) if m.present.iter().any(|q| q == p) => {
            (Verification::Confirmed, None)
        }
        // It holds no current evidence of the person: as if it could not be asked (§5.1).
        (CallOutcome::Members(m), Some(p)) if m.unknown.iter().any(|q| q == p) => {
            (Verification::Unanswered, Some("identity.unknown".into()))
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
    /// For a set: the members who delegated it found in it, and those found not to be.
    members: (Vec<String>, Vec<String>),
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
        let delegated = match binding.subject {
            Principal::ExternalSet { .. } => self.store.members_who_delegated(binding.id).await?,
            _ => Vec::new(),
        };
        let (request, person) = asked(&binding, &delegated)?;
        let target = CallTarget {
            organization: binding.provider.organization,
            kind: ProviderType::Identity,
            name: binding.provider.name.clone(),
        };
        let called = self
            .call_own_provider(&target, CallRequest::VerifyMembers(request), VERIFY_CALL)
            .await?;
        let members = members_found(&called.outcome, &delegated);
        let (verification, reason) = found(called.outcome, person);
        let answer = Answer { verification, reason, actor, relay, grant, members };
        if let Some(recorded) = self.record_answer(binding.clone(), &answer).await? {
            return Ok((recorded, answer.reason));
        }
        // Another answer was recorded since this one was asked. That a subject ended is always
        // safe to say, so it is said whatever was recorded, and asked again within a minute in
        // case it was older; that one is still there may be older than what was recorded, so it
        // is dropped.
        if verification == Verification::Ended || !answer.members.1.is_empty() {
            self.record_ended(binding.id, &answer).await?;
        }
        let current = self.store.binding(binding.id).await?.filter(|b| b.retired_at.is_none());
        let gone = || AppError::conflict("binding.retired", "the binding was retired");
        Ok((current.ok_or_else(gone)?, answer.reason))
    }

    /// Records that `id`'s subject ended, whatever was recorded meanwhile, with an effect if it
    /// was verified, and that the members found not in it are not.
    async fn record_ended(&self, id: Uuid, answer: &Answer) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        // A member found not in the set is recorded so whatever came between, as an ending is.
        let now = self.clock.now();
        tx.record_members(id, (&[], &answer.members.1), now).await?;
        if answer.verification == Verification::Ended {
            let before = tx.record_ended(id, now + RETRY).await?;
            if before == Some(BindingState::Verified) {
                let effect = transition(id, None, BindingState::Unverified, answer);
                tx.append_effect(&effect).await?;
            }
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
        tx.record_members(binding.id, (&answer.members.0, &answer.members.1), now).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::extension::identity::Members;

    fn record(user: &str) -> MemberRecord {
        MemberRecord {
            binding: Uuid::nil(),
            member: Principal::ExternalUser { provider: "org/corp".into(), user: user.into() },
            state: BindingState::Verified,
            last_verified_at: None,
        }
    }

    fn members(set_exists: bool, present: &[&str], unknown: &[&str]) -> CallOutcome {
        let owned = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect();
        CallOutcome::Members(Members {
            set_exists,
            present: owned(present),
            unknown: owned(unknown),
        })
    }

    #[test]
    fn a_member_the_provider_cannot_tell_about_is_neither_confirmed_nor_ended() {
        let asked = [record("ann"), record("bob"), record("cat")];
        let key = |u: &str| record(u).member.key();
        let (present, absent) = members_found(&members(true, &["ann"], &["bob"]), &asked);
        assert_eq!((present, absent), (vec![key("ann")], vec![key("cat")]));
        let (present, absent) = members_found(&members(false, &["ann"], &["bob"]), &asked);
        assert!(present.is_empty(), "a set that is gone holds no one");
        assert_eq!(absent.len(), 3);
        let (state, reason) = found(members(true, &[], &["ann"]), Some("ann"));
        assert_eq!(
            (state, reason.as_deref()),
            (Verification::Unanswered, Some("identity.unknown"))
        );
        assert_eq!(found(members(true, &[], &[]), Some("ann")).0, Verification::Ended);
    }
}
