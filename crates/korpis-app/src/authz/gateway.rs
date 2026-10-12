// SPDX-License-Identifier: AGPL-3.0-or-later

//! The gateway: authentication, and the one authorization path (P6, §3 of
//! docs/08-identity.md). Every use case authorizes through [`App::authorize`]; nothing else
//! decides.

use super::Snapshot;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, Tx};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::credential::Secret;
use korpis_core::authz::{self, Action, Decision, Grant, Kind, ObjectRef, Permit, Request};
use korpis_core::principal::Principal;
use korpis_core::workload::plan::Proposing;
use std::net::IpAddr;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Authenticates an API token secret presented from `source_ip`.
    pub async fn authenticate(
        &self,
        presented: &str,
        source_ip: Option<IpAddr>,
    ) -> Result<Caller, AppError> {
        let secret = Secret::presented(presented).ok_or(AppError::Unauthenticated)?;
        let token = self
            .store
            .authenticate(&secret.digest(), self.clock.now())
            .await?
            .ok_or(AppError::Unauthenticated)?;
        Ok(Caller::new(Principal::Token(token.id), source_ip, false))
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
        let Some(resource) = authz::Hierarchy::resource(&snapshot, object) else {
            return Err(AppError::NotFound { kind: object.kind.as_str() });
        };
        let request = Request {
            subject: &caller.subject,
            action,
            resource: &resource,
            source_ip: caller.source_ip,
            mfa: caller.mfa,
            now: self.clock.now(),
        };
        Ok(authz::evaluate(&request, &chains, &snapshot))
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
            Decision::Permit(permit) => Ok(permit),
            Decision::Propose { .. } | Decision::Deny(_) => {
                Err(self.refuse(caller, &action, object).await)
            }
        }
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
            Decision::Permit(permit) => Ok((permit, Proposing::Permitted)),
            Decision::Propose { permit, approval_scope } => {
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

    pub(crate) async fn record_denial(
        &self,
        caller: &Caller,
        action: &Action,
        object: ObjectRef,
    ) -> Result<(), AppError> {
        let mut effect = Effect::new(caller.principal.clone(), action.as_str(), Outcome::Denied)
            .error(details([
                ("missing_action", action.as_str().into()),
                ("object_kind", object.kind.as_str().into()),
                ("object", object.id.to_string().into()),
            ]));
        effect.workload = (object.kind == Kind::Workload).then_some(object.id);
        effect.node = (object.kind == Kind::Node).then_some(object.id);
        let mut tx = self.store.begin().await?;
        tx.append_effect(&effect).await?;
        tx.commit().await?;
        Ok(())
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
