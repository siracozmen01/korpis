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
use korpis_core::authz::{self, Action, Decision, Grant, Kind, ObjectRef, Permit, Request};
use korpis_core::principal::Principal;
use korpis_core::workload::plan::Proposing;
use std::net::IpAddr;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Authenticates a secret presented from `source_ip`: an API token's, or a running
    /// workload's credential (§9.5 of docs/08-identity.md).
    pub async fn authenticate(
        &self,
        presented: &str,
        source_ip: Option<IpAddr>,
    ) -> Result<Caller, AppError> {
        let (kind, secret) = Secret::presented_any(presented).ok_or(AppError::Unauthenticated)?;
        let now = self.clock.now();
        let principal = match kind {
            SecretKind::ApiToken => {
                let token = self.store.authenticate(&secret.digest(), now).await?;
                Principal::Token(token.ok_or(AppError::Unauthenticated)?.id)
            }
            SecretKind::Workload => {
                let held = self.store.authenticate_workload(&secret.digest(), now).await?;
                held.ok_or(AppError::Unauthenticated)?.subject
            }
        };
        Ok(Caller::new(principal, source_ip, false))
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
        let mut snapshot = Snapshot::default();
        snapshot.load(&self.store, object).await?;
        let chains = self.held_chains(caller, via, &mut snapshot).await?;
        self.evaluate(caller, action, object, &chains, &snapshot)
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
        for records in self.store.chains_held_by(&caller.subject.key).await? {
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
        action: &Action,
        object: ObjectRef,
        chains: &[Vec<Grant>],
        snapshot: &Snapshot,
    ) -> Option<Decision> {
        let resource = authz::Hierarchy::resource(snapshot, object)?;
        let request = Request {
            subject: &caller.subject,
            action,
            resource: &resource,
            source_ip: caller.source_ip,
            mfa: caller.mfa,
            now: self.clock.now(),
        };
        Some(authz::evaluate(&request, chains, snapshot))
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
        let action = core_action(action)?;
        let mut snapshot = Snapshot::default();
        let chains = self.held_chains(caller, None, &mut snapshot).await?;
        let mut out = Vec::with_capacity(objects.len());
        for o in objects {
            snapshot.load(&self.store, *o).await?;
            let decision = self.evaluate(caller, &action, *o, &chains, &snapshot);
            out.push(matches!(decision, Some(Decision::Permit(_))));
        }
        Ok(out)
    }

    /// Whether `caller` may do `action` on `object` now, without recording anything.
    pub(crate) async fn permitted(
        &self,
        caller: &Caller,
        action: &str,
        object: ObjectRef,
    ) -> Result<bool, AppError> {
        let action = core_action(action)?;
        match self.decide(caller, &action, object, None).await {
            Ok(d) => Ok(matches!(d, Decision::Permit(_))),
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

    /// Spends one use of a use-limited grant that permitted a call, by compare-and-swap, before
    /// the call proceeds (§6.2 of docs/08-identity.md). A use is spent when the call is
    /// authorized, whether or not the call then succeeds, so a grant is never used more often
    /// than it allows; a grant whose last use a concurrent call took refuses this one.
    async fn redeem(
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
fn read_namespace(kind: Kind) -> &'static str {
    match kind {
        Kind::Organization => "org",
        Kind::Project => "project",
        Kind::Workload | Kind::Stream => "workload",
        Kind::Volume => "volume",
        Kind::Endpoint => "endpoint",
        Kind::Node => "node",
    }
}
