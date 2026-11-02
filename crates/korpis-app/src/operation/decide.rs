// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deciding about an operation: approving it, pausing, resuming, or aborting it, each under the
//! action that decides about it. Approving takes a person acting directly, outside a portal, since
//! the control plane acts later as them: what reached them through a relay or a portal session is
//! not there when it does. A drain or an evacuation puts its node out of placement as it is
//! approved, and its end, however it ends, leaves the node cordoned. An abort begins nothing more;
//! a plan already applying goes on, and is the operator's to end through its own controls.

use super::OperationView;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite as _, NodeWrite as _, OperationWrite as _, RecipeSource, Store,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::operation::{
    Approved, Item, ItemState, Kind, Operation, Scope, TransitionError,
};
use std::time::SystemTime;
use uuid::Uuid;

const OPERATION: &str = "operation.decide_operation";

/// What is decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Approves it.
    Approve {
        /// The earliest it begins; `None` for at once.
        start_after: Option<SystemTime>,
        /// For a rollout, the grant the approver holds that each grant it issues is attenuated
        /// from.
        parent: Option<Uuid>,
    },
    /// Pauses it.
    Pause,
    /// Resumes it.
    Resume,
    /// Aborts it, for a reason.
    Abort(String),
}

impl Decision {
    const fn action(&self) -> &'static str {
        match self {
            Self::Approve { .. } => "operation.approve",
            Self::Pause => "operation.pause",
            Self::Resume => "operation.resume",
            Self::Abort(_) => "operation.abort",
        }
    }
}

/// A decision about an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecideOperation {
    /// The operation.
    pub operation: Uuid,
    /// The decision.
    pub decision: Decision,
}

fn refused(e: TransitionError) -> AppError {
    AppError::conflict("operation.status", e.to_string())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Decides about an operation.
    pub async fn decide_operation(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, OperationView>,
        cmd: DecideOperation,
    ) -> Result<Reply<OperationView>, AppError> {
        caller.direct()?;
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let found = self.store.operation(cmd.operation).await?;
        let (op, _) = found.ok_or(AppError::NotFound { kind: "operation" })?;
        let (action, object) = self.deciding(op.kind, op.scope).await?;
        let permit = self.authorize(caller, action, object).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let locked = tx.operation_in(cmd.operation).await?;
        let (mut op, mut items) = locked.ok_or(AppError::NotFound { kind: "operation" })?;
        let read_at = op.version;
        let view = (&mut op, &mut items);
        self.decided(&mut tx, (caller, permit.grant), view, &cmd.decision, now).await?;
        for item in &items {
            tx.put_item(item).await?;
        }
        if !tx.update_operation(&op, read_at).await? {
            return Err(AppError::Internal(format!("operation {} moved while locked", op.id)));
        }
        let mut e = caller.effect(cmd.decision.action(), Outcome::Succeeded).under(permit.grant);
        e = e.about(object).after(details([
            ("operation", op.id.to_string().into()),
            ("status", op.status.as_str().into()),
        ]));
        if let Scope::Node(n) = op.scope {
            e.node = Some(n);
        }
        tx.append_effect(&e).await?;
        op.version = read_at + 1;
        finish(tx, caller, idem, OPERATION, (op, items)).await
    }

    /// Applies `decision`, made by `caller` under `permitted`, to `op` and its `items` at `now`.
    async fn decided(
        &self,
        tx: &mut S::Tx,
        (caller, permitted): (&Caller, Uuid),
        (op, items): (&mut Operation, &mut Vec<Item>),
        decision: &Decision,
        now: SystemTime,
    ) -> Result<(), AppError> {
        match decision {
            Decision::Approve { start_after, parent } => {
                let under = self.approving_under(caller, op, (permitted, *parent)).await?;
                let approved = Approved {
                    by: caller.principal.clone(),
                    under,
                    mfa: caller.mfa,
                    at: now,
                    start_after: *start_after,
                };
                op.approve(approved).map_err(refused)?;
                self.take_node(tx, op).await?;
                self.items_again(tx, op, items, now).await?;
            }
            Decision::Pause => {
                op.pause(&format!("paused by {}", caller.principal.key())).map_err(refused)?;
            }
            Decision::Resume => op.resume().map_err(refused)?,
            Decision::Abort(why) => {
                op.abort(why, now).map_err(refused)?;
                for item in items.iter_mut().filter(|i| i.state == ItemState::Waiting) {
                    item.end(ItemState::Skipped, "the operation was aborted", now);
                }
                self.release_node(tx, op).await?;
            }
        }
        Ok(())
    }

    /// The grant `op`'s approval is carried out under: for a rollout, `parent`, a grant the
    /// approver holds; for the rest, the one that permitted the approval, `permitted`.
    async fn approving_under(
        &self,
        caller: &Caller,
        op: &Operation,
        (permitted, parent): (Uuid, Option<Uuid>),
    ) -> Result<Uuid, AppError> {
        if caller.within.is_some() {
            let why = "an operation is approved by a person signed in directly, not at a portal";
            return Err(AppError::conflict("operation.portal", why));
        }
        if op.kind != Kind::TemplateRollout {
            return Ok(permitted);
        }
        let parent = parent.ok_or_else(|| {
            AppError::invalid("parent", "the grant you hold that the rollout's grants come from")
        })?;
        let chain = self.store.grant_chain(parent).await?;
        let held = chain.first().is_some_and(|r| r.grant.subject.key == caller.subject.key);
        if !held {
            return Err(AppError::invalid("parent", "a grant you hold"));
        }
        Ok(parent)
    }

    /// Puts `op`'s node, if it is over one, out of placement as `op` is approved.
    async fn take_node(&self, tx: &mut S::Tx, op: &Operation) -> Result<(), AppError> {
        let Scope::Node(id) = op.scope else { return Ok(()) };
        let mut node = tx.lock_node(id).await?.ok_or(AppError::NotFound { kind: "node" })?;
        let expected = node.version;
        let changed =
            node.drain().map_err(|e| AppError::conflict("node.removed", e.to_string()))?;
        if changed && !tx.update_node_admin(&node, expected).await? {
            return Err(AppError::Internal(format!("node {id} moved while locked")));
        }
        Ok(())
    }

    /// Leaves `op`'s node, if it is over one, cordoned as `op` ends.
    pub(super) async fn release_node(
        &self,
        tx: &mut S::Tx,
        op: &Operation,
    ) -> Result<(), AppError> {
        let Scope::Node(id) = op.scope else { return Ok(()) };
        let Some(mut node) = tx.lock_node(id).await? else { return Ok(()) };
        let expected = node.version;
        if node.drained() && !tx.update_node_admin(&node, expected).await? {
            return Err(AppError::Internal(format!("node {id} moved while locked")));
        }
        Ok(())
    }
}
