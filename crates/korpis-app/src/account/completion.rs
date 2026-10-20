// SPDX-License-Identifier: AGPL-3.0-or-later

//! Signing in through an identity provider, completed (§9.7 of docs/08-identity.md). The person
//! returns with the state, which is spent once, and a code the provider asserts with the verifier
//! the panel kept; the presentation is accepted once; the person's binding is made at their first
//! use or confirmed; their records as members are confirmed in the bound sets the provider named,
//! and not in any other of its sets; and a session begins, or, for a caller signed in through the
//! same provider as the same person, their session re-authenticates. Every refusal of what the
//! person brought is [`AppError::Unauthenticated`], a failed attempt the server charges to its
//! source (§10 of docs/10-api.md).

use super::external::{target, unavailable};
use super::{Begun, Client, record_held};
use crate::App;
use crate::authz::first_use;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{
    AssertionWrite as _, Clock, MemberWrite as _, ProviderCallWrite as _, RecipeSource,
    SignInWrite as _, Store, Tx as _,
};
use korpis_core::authz::binding::{IdentityBinding, ProviderRef, Verification};
use korpis_core::authz::sign_in::{
    MAX_CODE, PendingSignIn, confirmed_in, is_verifier, redirect_uri, state_digest, verifies,
};
use korpis_core::authz::ticket::fresh;
use korpis_core::extension::identity::{Assertion, Evidence};
use korpis_core::extension::{CallOutcome, CallRequest};
use korpis_core::principal::{Holder, Principal, SetName};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long an identity provider has to redeem a code and say whose it was: a person is waiting.
const ASSERT_CALL: Duration = Duration::from_secs(10);

/// What a person returns to the panel's callback with, and the verifier the panel kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The state the identity provider returned.
    pub state: String,
    /// The authorization code it returned.
    pub code: String,
    /// The PKCE verifier the panel kept.
    pub code_verifier: String,
}

/// A sign-in through an identity provider, completed.
#[derive(Debug)]
pub enum Completed {
    /// A session began, held by the person's binding.
    Begun {
        /// The person's binding.
        binding: Uuid,
        /// The session, and its refresh credential.
        session: Begun,
    },
    /// The caller's own session re-authenticated.
    Reauthenticated {
        /// The person's binding.
        binding: Uuid,
        /// The session.
        session: Uuid,
        /// When.
        at: SystemTime,
    },
}

/// The provider's answer, believed only of a presentation now; its refusal of the code is the
/// person's failed attempt, and the control plane's failure to reach it fails closed.
fn believed(outcome: CallOutcome, now: SystemTime) -> Result<Assertion, AppError> {
    match outcome {
        CallOutcome::Asserted(a) if fresh(a.at, now) => Ok(a),
        CallOutcome::Failed(f) if f.reason.starts_with("provider.") => {
            Err(unavailable(CallOutcome::Failed(f)))
        }
        CallOutcome::Asserted(_) | CallOutcome::Failed(_) => Err(AppError::Unauthenticated),
        other => Err(unavailable(other)),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Completes the sign-in `c` returns to, from `client`: a session for the person, or, for
    /// `caller` signed in as the same person, its session re-authenticated.
    pub async fn complete_external_sign_in(
        &self,
        caller: Option<&Caller>,
        c: Completion,
        client: &Client,
    ) -> Result<Completed, AppError> {
        if c.code.is_empty() || c.code.len() > MAX_CODE {
            return Err(AppError::invalid("code", "the code the identity provider returned"));
        }
        if !is_verifier(&c.code_verifier) {
            return Err(AppError::invalid("code_verifier", "a PKCE verifier, RFC 7636 §4.1"));
        }
        let pending = self.spend_sign_in(&c.state).await?;
        if !verifies(&c.code_verifier, &pending.code_challenge) {
            return Err(AppError::Unauthenticated);
        }
        let assertion = self.assert_sign_in(&pending, c).await?;
        let provider = &pending.provider;
        let person = Principal::ExternalUser { provider: provider.key(), user: assertion.user };
        person.validate().map_err(|_| AppError::Unauthenticated)?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        if !tx.accept_presentation(provider, &assertion.presentation, now).await? {
            return Err(AppError::Unauthenticated);
        }
        let binding = self.person_binding(&mut tx, provider, &person, now).await?;
        let confirmed = self.confirmed_sets(provider, &assertion.sets).await?;
        tx.record_sign_in(&person.key(), provider, &confirmed, now).await?;
        let holder = Holder::Person(binding);
        let more = [
            ("provider", provider.key().into()),
            ("presentation", assertion.presentation.clone().into()),
            ("sets", confirmed.len().into()),
        ];
        record_held(&mut tx, &person, "identity.signed_in", holder, more).await?;
        let completed = match caller {
            None => {
                let held = (holder, person);
                let session = self.begin_held(&mut tx, held, assertion.sets, client).await?;
                Completed::Begun { binding, session }
            }
            Some(caller) => reauthenticated(&mut tx, caller, holder, now).await?,
        };
        tx.commit().await?;
        Ok(completed)
    }

    /// Spends the sign-in `state` finds, once, whatever happens after: a state is good for one
    /// completion. Unauthenticated if it finds none unexpired.
    async fn spend_sign_in(&self, state: &str) -> Result<PendingSignIn, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let spent = tx.spend_pending_sign_in(&state_digest(state), now).await?;
        tx.commit().await?;
        spent.ok_or(AppError::Unauthenticated)
    }

    /// Has `pending`'s provider redeem `c`'s code and say whose it was; the call keeps neither
    /// the code nor the verifier once answered.
    async fn assert_sign_in(
        &self,
        pending: &PendingSignIn,
        c: Completion,
    ) -> Result<Assertion, AppError> {
        let origin = self.panel_origin().ok_or_else(|| AppError::Unavailable {
            what: "signing in through an identity provider",
            detail: "no --public-url is configured, so there is no panel to return to".into(),
        })?;
        let headers = BTreeMap::from([
            ("code_verifier".to_owned(), c.code_verifier),
            ("redirect_uri".to_owned(), redirect_uri(origin)),
            ("nonce".to_owned(), pending.nonce.clone()),
        ]);
        let request = CallRequest::Assert(Evidence { body: c.code.into_bytes(), headers });
        let target = target(&pending.provider);
        let called = self.call_own_provider(&target, request, ASSERT_CALL).await?;
        let mut tx = self.store.begin().await?;
        tx.redact_sign_in_call(called.id).await?;
        tx.commit().await?;
        believed(called.outcome, self.clock.now())
    }

    /// The live binding of `person`, made at their first use or confirmed now: the provider has
    /// just vouched for them. Unauthenticated if an operator retired it.
    async fn person_binding(
        &self,
        tx: &mut S::Tx,
        provider: &ProviderRef,
        person: &Principal,
        now: SystemTime,
    ) -> Result<Uuid, AppError> {
        let Some(mut bound) = self.store.binding_of(&person.key()).await? else {
            let first = first_use(provider, person, now);
            if !tx.bind_at_first_use(&first).await? {
                return Err(AppError::Unauthenticated);
            }
            return Ok(first.id);
        };
        confirm(tx, &mut bound, now).await?;
        Ok(bound.id)
    }

    /// The bindings of the sets of `provider` in `named` that a sign-in confirms its person in.
    async fn confirmed_sets(
        &self,
        provider: &ProviderRef,
        named: &[SetName],
    ) -> Result<Vec<Uuid>, AppError> {
        let mut bound = Vec::new();
        for s in named {
            let set = Principal::ExternalSet {
                provider: provider.key(),
                realm: s.realm.clone(),
                member_of: s.member_of.clone(),
            };
            if let Some(b) = self.store.binding_of(&set.key()).await? {
                bound.push(b);
            }
        }
        Ok(confirmed_in(named, &bound).into_iter().map(|b| b.id).collect())
    }
}

/// Records `bound` confirmed at `now`, due again after its interval; a re-verification recorded
/// meanwhile stands instead.
async fn confirm<T: crate::ports::Tx>(
    tx: &mut T,
    bound: &mut IdentityBinding,
    now: SystemTime,
) -> Result<(), AppError> {
    let asked_at = bound.verifications;
    bound.verified(Verification::Confirmed, now);
    let due = now + bound.terms.reverify_interval;
    tx.record_verification((bound.id, asked_at), bound.state, bound.last_verified_at, due).await?;
    Ok(())
}

/// Re-authenticates `caller`'s session, if `holder` holds it: only the same person's sign-in
/// vouches for their session.
async fn reauthenticated<T: crate::ports::Tx>(
    tx: &mut T,
    caller: &Caller,
    holder: Holder,
    now: SystemTime,
) -> Result<Completed, AppError> {
    let (Some(held), Some(session)) = (caller.holder, caller.session) else {
        return Err(AppError::Unauthenticated);
    };
    if held != holder || !tx.reauthenticated(holder, session, now).await? {
        return Err(AppError::Unauthenticated);
    }
    let Holder::Person(binding) = holder else { return Err(AppError::Unauthenticated) };
    let more = [("session", session.to_string().into()), ("factor", "provider".into())];
    record_held(tx, &caller.principal, "session.reauthenticated", holder, more).await?;
    Ok(Completed::Reauthenticated { binding, session, at: now })
}
