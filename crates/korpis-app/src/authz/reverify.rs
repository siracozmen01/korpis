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
use korpis_core::principal::{Principal, SetName};
use std::time::Duration;
use uuid::Uuid;

/// How long an identity provider has to answer a re-verification.
const VERIFY_CALL: Duration = Duration::from_secs(15);

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

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Asks `binding`'s provider about it once and records what it found, with an effect by
    /// `actor` (under `grant`, when a person asked) if its state changed: the binding as it now
    /// stands, and the provider's reason when it could not answer.
    pub(crate) async fn reverify(
        &self,
        mut binding: IdentityBinding,
        actor: Principal,
        grant: Option<Uuid>,
    ) -> Result<(IdentityBinding, Option<String>), AppError> {
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
        let changed = binding.verified(verification, self.clock.now());
        let mut tx = self.store.begin().await?;
        let state = binding.state;
        if !tx.record_verification(binding.id, state, binding.last_verified_at).await? {
            return Err(AppError::conflict("binding.retired", "the binding was retired"));
        }
        if changed {
            let action = match state {
                BindingState::Verified => "identity.binding.verified",
                BindingState::Unverified => "identity.binding.unverified",
            };
            let mut effect = Effect::new(actor, action, Outcome::Succeeded).after(details([
                ("binding", binding.id.to_string().into()),
                ("subject", binding.subject.key().into()),
                ("reason", reason.clone().into()),
            ]));
            if let Some(g) = grant {
                effect = effect.under(g);
            }
            tx.append_effect(&effect).await?;
        }
        tx.commit().await?;
        Ok((binding, reason))
    }

    /// Re-verifies a binding now, under `identity.binding.write` where it is declared, rather
    /// than at its next interval.
    pub async fn verify_identity_binding(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<(IdentityBinding, Option<String>), AppError> {
        let (binding, permit) = self.binding_to_write(caller, id).await?;
        self.reverify(binding, caller.principal.clone(), Some(permit.grant)).await
    }
}
