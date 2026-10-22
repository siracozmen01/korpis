// SPDX-License-Identifier: AGPL-3.0-or-later

//! The gateway: authentication, and the one authorization path (P6, §3 of
//! docs/08-identity.md). Every use case authorizes through [`App::authorize`]; nothing else
//! decides.

use super::Snapshot;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, GrantWrite, RecipeSource, Store, Tx};
use korpis_core::authz::credential::{Secret, SecretKind};
use korpis_core::authz::token::KeyRing;
use korpis_core::authz::{self, Action, Decision, Denial, Grant, Kind, ObjectRef, Permit, Request};
use korpis_core::authz::{access, link};
use korpis_core::principal::{Holder, Principal};
use korpis_core::workload::plan::Proposing;
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Authenticates a secret presented from `source_ip`: an API token's, a running workload's
    /// credential (§9.5 of docs/08-identity.md), or a share link (§6.1).
    pub async fn authenticate(
        &self,
        presented: &str,
        source_ip: Option<IpAddr>,
    ) -> Result<Caller, AppError> {
        let now = self.clock.now();
        if presented.starts_with(access::ACCESS_PREFIX)
            || presented.starts_with(access::PERSON_ACCESS_PREFIX)
        {
            // An account's is verified by its signature and expiry alone (§9.4 of
            // docs/08-identity.md): what the user may do is evaluated per call, from the grants it
            // holds then. A person's names a session the gateway reads (§9.7).
            let claims = access::verify(&self.token_ring()?, presented, now)
                .map_err(|_| AppError::Unauthenticated)?;
            return match claims.holder {
                Holder::Account(user) => {
                    let mut caller = Caller::new(Principal::User(user), source_ip, true)
                        .with_session(claims.holder, claims.session);
                    caller.within = claims.within;
                    Ok(caller)
                }
                Holder::Person(_) => self.person(&claims, source_ip, now).await,
            };
        }
        if presented.starts_with(link::LINK_PREFIX) {
            let grant = self.linked(presented, now).await?;
            return Ok(Caller::new(Principal::Link(grant), source_ip, false));
        }
        let (kind, secret) = Secret::presented_any(presented).ok_or(AppError::Unauthenticated)?;
        let principal = match kind {
            SecretKind::ApiToken => {
                let token = self.store.authenticate(&secret.digest(), now).await?;
                Principal::Token(token.ok_or(AppError::Unauthenticated)?.id)
            }
            SecretKind::Workload => {
                let held = self.store.authenticate_workload(&secret.digest(), now).await?;
                held.ok_or(AppError::Unauthenticated)?.subject
            }
            // Never presented as the bearer of a call; `presented_any` does not offer them.
            SecretKind::Invitation
            | SecretKind::Refresh
            | SecretKind::DeviceCode
            | SecretKind::Ticket => {
                return Err(AppError::Unauthenticated);
            }
        };
        Ok(Caller::new(principal, source_ip, false))
    }

    /// The link-form grant a share link is the bearer form of, while it is one that works: signed
    /// by this control plane's current token key, not expired, and naming a grant that exists
    /// unrevoked. What it permits, and the use each call spends, is evaluated like any grant.
    async fn linked(&self, presented: &str, now: SystemTime) -> Result<Uuid, AppError> {
        let keys = self.token_ring()?;
        let found = link::verify(presented, &keys, now).map_err(|_| AppError::Unauthenticated)?;
        let chain = self.store.grant_chain(found.grant).await?;
        let works = chain.first().is_some_and(|leaf| {
            leaf.grant.link_form
                && leaf.holder == Principal::Link(found.grant)
                && !leaf.grant.revoked
        });
        if works { Ok(found.grant) } else { Err(AppError::Unauthenticated) }
    }

    /// The keys bearers this control plane signed are verified with: its current token key.
    pub(crate) fn token_ring(&self) -> Result<KeyRing, AppError> {
        let mut keys = KeyRing::default();
        keys.insert(self.token_key.key_id, &self.token_key.public_bytes())
            .map_err(|e| AppError::Internal(e.to_string()))?;
        Ok(keys)
    }

    /// Evaluates `action` on `object` for `caller` without recording anything; through only the
    /// grant `via` when it is given.
    pub(crate) async fn decide(
        &self,
        caller: &Caller,
        action: &Action,
        object: ObjectRef,
        via: Option<Uuid>,
    ) -> Result<Decision, AppError> {
        self.decide_as(caller, (action, object), via, false).await
    }

    /// [`Self::decide`], for a renewal when `renewal` is set: a session token renewed whose use
    /// was spent when the first was minted (§6.1 of docs/08-identity.md).
    pub(crate) async fn decide_as(
        &self,
        caller: &Caller,
        (action, object): (&Action, ObjectRef),
        via: Option<Uuid>,
        renewal: bool,
    ) -> Result<Decision, AppError> {
        let mut snapshot = Snapshot::default();
        snapshot.load(&self.store, object).await?;
        let chains = self.held_chains(caller, via, &mut snapshot).await?;
        self.evaluate(caller, (action, object), &chains, &snapshot, renewal)
            .ok_or(AppError::NotFound { kind: object.kind.as_str() })
    }

    /// The caller's grant chains, through only the grant `via` when it is given, with what each
    /// depends on loaded into `snapshot`.
    pub(crate) async fn held_chains(
        &self,
        caller: &Caller,
        via: Option<Uuid>,
        snapshot: &mut Snapshot,
    ) -> Result<Vec<Vec<Grant>>, AppError> {
        let mut chains = Vec::new();
        let mut held = self.store.chains_held_by(&caller.subject.key).await?;
        // A person acting through a relay holds, for the call, what the sets the assertion
        // names hold (§5.2 of docs/08-identity.md).
        for set in &caller.memberships {
            held.extend(self.store.chains_held_by(&set.key).await?);
        }
        for records in held {
            if via.is_some_and(|v| records.first().is_none_or(|leaf| leaf.grant.id != v)) {
                continue;
            }
            let chain: Vec<Grant> = records.into_iter().map(|r| r.grant).collect();
            for g in &chain {
                snapshot.load_grant(&self.store, g).await?;
            }
            chains.push(chain);
        }
        Ok(chains)
    }

    /// One decision over a loaded snapshot; `None` if the object does not exist.
    fn evaluate(
        &self,
        caller: &Caller,
        (action, object): (&Action, ObjectRef),
        chains: &[Vec<Grant>],
        snapshot: &Snapshot,
        renewal: bool,
    ) -> Option<Decision> {
        let resource = authz::Hierarchy::resource(snapshot, object)?;
        if !caller.may_ask(action, &resource) {
            let missing_action = action.clone();
            return Some(Decision::Deny(Denial { missing_action, reasons: Vec::new() }));
        }
        let request = Request {
            subject: &caller.subject,
            memberships: &caller.memberships,
            action,
            resource: &resource,
            source_ip: caller.source_ip,
            mfa: caller.mfa,
            now: self.clock.now(),
            renewal,
        };
        Some(authz::evaluate(&request, chains, snapshot))
    }

    /// Refuses `object` if it lies outside where the caller may act ([`Caller::confined`]): for
    /// what decides without the one decision path, such as revoking a grant one holds.
    pub(crate) async fn confine(&self, caller: &Caller, object: ObjectRef) -> Result<(), AppError> {
        if caller.within.is_none() {
            return Ok(());
        }
        let mut snapshot = Snapshot::default();
        snapshot.load(&self.store, object).await?;
        caller.confined(authz::Hierarchy::resource(&snapshot, object).as_ref())
    }

    /// Whether each of `objects` lies where the caller may act ([`Caller::reaches`]): what a
    /// list of the caller's own, such as the grants it holds, answers a session begun at a
    /// portal (§9.8 of docs/08-identity.md).
    pub(crate) async fn reached_each(
        &self,
        caller: &Caller,
        objects: &[ObjectRef],
    ) -> Result<Vec<bool>, AppError> {
        if caller.within.is_none() {
            return Ok(vec![true; objects.len()]);
        }
        let mut snapshot = Snapshot::default();
        let mut reached = Vec::with_capacity(objects.len());
        for o in objects {
            snapshot.load(&self.store, *o).await?;
            let resource = authz::Hierarchy::resource(&snapshot, *o);
            reached.push(resource.is_some_and(|r| caller.reaches(&r)));
        }
        Ok(reached)
    }

    /// Whether `caller` may do `action` on each of `objects` now, without recording anything:
    /// the caller's grants are loaded once for all of them, which is what filtering a list or
    /// an explanation per viewer needs (§8 of docs/08-identity.md).
    pub(crate) async fn permitted_each(
        &self,
        caller: &Caller,
        action: &str,
        objects: &[ObjectRef],
    ) -> Result<Vec<bool>, AppError> {
        let asked: Vec<(&str, ObjectRef)> = objects.iter().map(|o| (action, *o)).collect();
        self.permitted_all(caller, &asked).await
    }

    /// [`Self::permitted_each`] for an action of each object's own, the caller's grants loaded
    /// once for all of them.
    pub(crate) async fn permitted_all(
        &self,
        caller: &Caller,
        asked: &[(&str, ObjectRef)],
    ) -> Result<Vec<bool>, AppError> {
        let mut snapshot = Snapshot::default();
        let chains = self.held_chains(caller, None, &mut snapshot).await?;
        let mut out = Vec::with_capacity(asked.len());
        for (action, o) in asked {
            let action = core_action(action)?;
            snapshot.load(&self.store, *o).await?;
            let decision = self.evaluate(caller, (&action, *o), &chains, &snapshot, false);
            out.push(matches!(decision, Some(Decision::Permit(p)) if !p.redeem));
        }
        Ok(out)
    }

    /// Whether `caller` may do `action` on `object` now, without recording anything. Nothing is
    /// spent here, so a use-limited grant permits nothing through it: a grant is used only where
    /// its use is counted (§6.2 of docs/08-identity.md).
    pub(crate) async fn permitted(
        &self,
        caller: &Caller,
        action: &str,
        object: ObjectRef,
    ) -> Result<bool, AppError> {
        let action = core_action(action)?;
        match self.decide(caller, &action, object, None).await {
            Ok(d) => Ok(matches!(d, Decision::Permit(p) if !p.redeem)),
            Err(AppError::NotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Authorizes an action performed now. A propose-only grant does not authorize it. A refusal
    /// is recorded as an effect, and a caller who may not even read the object is told it does
    /// not exist (§8 of docs/10-api.md).
    pub(crate) async fn authorize(
        &self,
        caller: &Caller,
        action: &str,
        object: ObjectRef,
    ) -> Result<Permit, AppError> {
        let action = core_action(action)?;
        match self.decide(caller, &action, object, None).await? {
            Decision::Permit(permit) => self.redeem(caller, &action, object, permit).await,
            Decision::Propose { .. } | Decision::Deny(_) => {
                Err(self.refuse(caller, &action, object).await)
            }
        }
    }

    /// Authorizes a call that continues one already authorized through the grant `through`, as
    /// each later read of a watch does: the grant is evaluated as it stands, so revocation,
    /// expiry, and its conditions still end it, but no use is spent and a grant whose last use
    /// was taken still permits it, because the call it continues spent the one it costs (§6.1
    /// of docs/08-identity.md). Only that grant is consulted, so the continuation is never paid
    /// for by another.
    pub(crate) async fn authorize_continued(
        &self,
        caller: &Caller,
        action: &str,
        object: ObjectRef,
        through: Uuid,
    ) -> Result<Permit, AppError> {
        let action = core_action(action)?;
        match self.decide_as(caller, (&action, object), Some(through), true).await? {
            Decision::Permit(permit) => Ok(permit),
            Decision::Propose { .. } | Decision::Deny(_) => {
                Err(self.refuse(caller, &action, object).await)
            }
        }
    }

    /// Spends one use of a use-limited grant that permitted a call, by compare-and-swap, before
    /// the call proceeds (§6.2 of docs/08-identity.md). A use is spent when the call is
    /// authorized, whether or not the call then succeeds, so a grant is never used more often
    /// than it allows; a grant whose last use a concurrent call took refuses this one.
    pub(crate) async fn redeem(
        &self,
        caller: &Caller,
        action: &Action,
        object: ObjectRef,
        permit: Permit,
    ) -> Result<Permit, AppError> {
        if !permit.redeem {
            return Ok(permit);
        }
        let chain = self.store.grant_chain(permit.grant).await?;
        let max = chain.first().and_then(|leaf| leaf.grant.conditions.max_uses);
        let Some(max) = max else { return Ok(permit) };
        let mut tx = self.store.begin().await?;
        if !tx.take_uses(permit.grant, 1, max).await? {
            drop(tx);
            return Err(self.refuse(caller, action, object).await);
        }
        tx.commit().await?;
        Ok(permit)
    }

    /// Authorizes a declaration: like [`App::authorize`], except that a propose-only grant
    /// authorizes it too, and the plan it produces then waits for an approver (§3.6 of
    /// docs/08-identity.md).
    pub(crate) async fn authorize_declaration(
        &self,
        caller: &Caller,
        action: &str,
        object: ObjectRef,
    ) -> Result<(Permit, Proposing), AppError> {
        let action = core_action(action)?;
        match self.decide(caller, &action, object, None).await? {
            Decision::Permit(permit) => {
                Ok((self.redeem(caller, &action, object, permit).await?, Proposing::Permitted))
            }
            Decision::Propose { permit, approval_scope } => {
                let permit = self.redeem(caller, &action, object, permit).await?;
                Ok((permit, Proposing::ProposeOnly { approval_scope }))
            }
            Decision::Deny(_) => Err(self.refuse(caller, &action, object).await),
        }
    }

    /// Records a refusal and returns what the caller is told.
    pub(crate) async fn refuse(
        &self,
        caller: &Caller,
        action: &Action,
        object: ObjectRef,
    ) -> AppError {
        if let Err(e) = self.record_denial(caller, action, object).await {
            return e;
        }
        let read = format!("{}.read", read_namespace(object.kind));
        match self.permitted(caller, &read, object).await {
            Ok(true) => AppError::Denied { action: action.as_str().to_owned() },
            Ok(false) => AppError::NotFound { kind: object.kind.as_str() },
            Err(e) => e,
        }
    }
}

/// An action the code names, which is always a valid one.
pub(crate) fn core_action(name: &str) -> Result<Action, AppError> {
    Action::parse(name).map_err(|e| AppError::Internal(format!("action {name}: {e}")))
}

/// The action namespace whose `.read` gates visibility of an object of `kind`.
pub(crate) fn read_namespace(kind: Kind) -> &'static str {
    match kind {
        Kind::Organization => "org",
        Kind::Project => "project",
        Kind::Workload | Kind::Stream => "workload",
        Kind::Volume => "volume",
        Kind::Endpoint => "endpoint",
        Kind::Node => "node",
        Kind::Repository => "repository",
        Kind::Domain => "domain",
        Kind::Certificate => "certificate",
        Kind::Portal => "portal",
    }
}
