// SPDX-License-Identifier: AGPL-3.0-or-later

//! Asserting an external identity (§5.2 of docs/08-identity.md). A client holding
//! `identity.relay` over a provider's organization passes on what a person presented; the
//! control plane has that provider, its own and no ancestor's, say who presented it, accepts the
//! presentation once, makes the person's binding at their first use, and hands the client a
//! ticket for the one action on the one object it named. The client holds nothing of the
//! person's beyond that ticket, and the ticket works only beside the client's own credential.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{AssertionWrite as _, Clock, EffectWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef,
};
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::authz::ticket::{PRESENTATION_KEPT, TICKET_LIFETIME, Ticket, fresh};
use korpis_core::authz::{Action, Kind, ObjectRef};
use korpis_core::extension::identity::{Assertion, Evidence};
use korpis_core::extension::{CallOutcome, CallRequest, ProviderType};
use korpis_core::principal::Principal;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// How long an identity provider has to say who presented something: a person is waiting.
const ASSERT_CALL: Duration = Duration::from_secs(10);

/// What a relaying client asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssertAsked {
    /// The provider to ask.
    pub provider: ProviderRef,
    /// What the person presented, as the client received it.
    pub evidence: Evidence,
    /// The one action the person asked for.
    pub action: Action,
    /// The one object they asked it of.
    pub object: ObjectRef,
}

/// What the client is handed.
#[derive(Debug)]
pub struct Asserted {
    /// The ticket, as kept.
    pub ticket: Ticket,
    /// Its secret, shown once.
    pub secret: Secret,
}

/// The provider's answer, believed only if it answers in time and of a presentation now.
fn believed(outcome: CallOutcome, now: SystemTime) -> Result<Assertion, AppError> {
    match outcome {
        CallOutcome::Asserted(a) if fresh(a.at, now) => Ok(a),
        CallOutcome::Asserted(_) => {
            Err(AppError::invalid("evidence", "it was presented too long ago, or ahead of now"))
        }
        // The control plane's own failures to reach the provider fail closed and say so; the
        // provider's refusal of the evidence is the provider's word.
        CallOutcome::Failed(f) if f.reason.starts_with("provider.") => {
            Err(AppError::Unavailable { what: "the identity provider", detail: f.reason })
        }
        CallOutcome::Failed(f) => Err(AppError::invalid("evidence", f.reason)),
        _ => Err(AppError::Unavailable {
            what: "the identity provider",
            detail: "provider.malformed".into(),
        }),
    }
}

/// The binding a person gets at their first authenticated use (§5): it decides nothing, and is
/// verified, since the provider has just vouched for them. Made once: a binding an operator
/// retired stays retired.
fn first_use(provider: &ProviderRef, subject: &Principal, now: SystemTime) -> IdentityBinding {
    IdentityBinding {
        id: Uuid::now_v7(),
        provider: provider.clone(),
        subject: subject.clone(),
        origin: BindingOrigin::FirstUse,
        terms: BindingTerms::default(),
        state: BindingState::Verified,
        last_verified_at: Some(now),
        verifications: 0,
        version: 1,
        made_by: subject.clone(),
        made_at: now,
        retired_at: None,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Has `asked`'s provider assert its evidence for `caller`, a relaying client, and hands it
    /// a ticket for the one action on the one object.
    pub async fn assert_identity(
        &self,
        caller: &Caller,
        asked: AssertAsked,
    ) -> Result<Asserted, AppError> {
        let p = &asked.provider;
        let organization = ObjectRef::new(Kind::Organization, p.organization);
        let permit = self.authorize(caller, "identity.relay", organization).await?;
        if !asked.evidence.bounded() {
            return Err(AppError::invalid("evidence", "at most 64 KiB and 32 headers"));
        }
        let target = CallTarget {
            organization: p.organization,
            kind: ProviderType::Identity,
            name: p.name.clone(),
        };
        let request = CallRequest::Assert(asked.evidence);
        let answered = self.call_own_provider(&target, request, ASSERT_CALL).await?;
        let now = self.clock.now();
        let assertion = believed(answered.outcome, now)?;
        let subject = Principal::ExternalUser { provider: p.key(), user: assertion.user.clone() };
        subject.validate().map_err(|e| AppError::invalid("evidence", e.to_string()))?;
        let secret = Secret::generate_kind(SecretKind::Ticket)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let ticket = Ticket {
            id: Uuid::now_v7(),
            relay: caller.principal.clone(),
            subject,
            sets: assertion.sets,
            provider: p.clone(),
            presentation: assertion.presentation,
            action: asked.action,
            object: asked.object,
            expires_at: now + TICKET_LIFETIME,
        };
        self.record_assertion(&ticket, &secret, permit.grant, now).await?;
        Ok(Asserted { ticket, secret })
    }

    /// Forgets the presentations accepted, and the tickets expired, more than a presentation is
    /// kept ago: by then each is refused as stale whatever is remembered of it. How many.
    pub async fn forget_assertions(&self) -> Result<u64, AppError> {
        let before = self.clock.now().checked_sub(PRESENTATION_KEPT).unwrap_or(UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        let forgotten = tx.forget_assertions(before).await?;
        tx.commit().await?;
        Ok(forgotten)
    }

    /// Accepts the ticket's presentation once, makes the person's binding at their first use,
    /// keeps the ticket, and records the assertion, in one transaction.
    async fn record_assertion(
        &self,
        ticket: &Ticket,
        secret: &Secret,
        grant: Uuid,
        now: SystemTime,
    ) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        if !tx.accept_presentation(&ticket.provider, &ticket.presentation, now).await? {
            let why = "that presentation was accepted already; a person presents again";
            return Err(AppError::conflict("identity.replayed", why));
        }
        tx.bind_at_first_use(&first_use(&ticket.provider, &ticket.subject, now)).await?;
        tx.insert_ticket(ticket, &secret.digest()).await?;
        let effect = Effect::new(ticket.relay.clone(), "identity.asserted", Outcome::Succeeded)
            .under(grant)
            .after(details([
                ("ticket", ticket.id.to_string().into()),
                ("subject", ticket.subject.key().into()),
                ("provider", ticket.provider.key().into()),
                ("presentation", ticket.presentation.clone().into()),
                ("action", ticket.action.as_str().into()),
                ("object_kind", ticket.object.kind.as_str().into()),
                ("object", ticket.object.id.to_string().into()),
            ]));
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(())
    }
}
