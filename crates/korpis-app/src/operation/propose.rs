// SPDX-License-Identifier: AGPL-3.0-or-later

//! Proposing an operation: what it will touch, found and kept, so the person who approves it reads
//! the decision actually made, item by item. Nothing changes until it is approved.

use super::OperationView;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite as _, OperationWrite as _, RecipeSource, Store, StoreError, Unique,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::node::{Connectivity, NodeAdmin};
use korpis_core::operation::{Kind, Operation, Policy, Scope, Status};
use uuid::Uuid;

const OPERATION: &str = "operation.propose_operation";

/// An operation to propose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposeOperation {
    /// What it does.
    pub kind: Kind,
    /// What it is over.
    pub scope: Scope,
    /// How it proceeds.
    pub policy: Policy,
}

/// The refusal of a second operation over a node that is under one.
pub(super) fn node_busy(e: StoreError) -> AppError {
    match e {
        StoreError::Duplicate(Unique::NodeOperation) => AppError::conflict(
            "operation.node_busy",
            "the node is under another operation that has not ended",
        ),
        other => other.into(),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Whether an operation of `kind` may be over `scope` now: a drain over a node that takes
    /// part, an evacuation over one that cannot, and a rollout of a version its template has.
    async fn proposable(&self, kind: Kind, scope: Scope) -> Result<(), AppError> {
        match scope {
            Scope::Node(id) => {
                let node = self.store.node(id).await?.ok_or(AppError::NotFound { kind: "node" })?;
                if node.admin == NodeAdmin::Decommissioned {
                    return Err(AppError::conflict("node.removed", "the node was removed"));
                }
                let heard = node.connectivity == Connectivity::Connected;
                match kind {
                    Kind::Drain if !heard => Err(AppError::conflict(
                        "node.unreachable",
                        "a drain moves workloads with the node's part in it; evacuate a node \
                         that cannot take part",
                    )),
                    Kind::Evacuate if node.connectivity != Connectivity::Unreachable => {
                        Err(AppError::conflict(
                            "node.reachable",
                            "an evacuation is for a node found unreachable; drain one that is not",
                        ))
                    }
                    _ => Ok(()),
                }
            }
            Scope::Template { template, version } => {
                let found = self.store.template_version(template, version).await?;
                found.map(|_| ()).ok_or(AppError::NotFound { kind: "grant template version" })
            }
        }
    }

    /// Proposes an operation, with every item it will touch.
    pub async fn propose_operation(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, OperationView>,
        cmd: ProposeOperation,
    ) -> Result<Reply<OperationView>, AppError> {
        caller.direct()?;
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        if !cmd.scope.fits(cmd.kind) {
            let why = "a node for a drain or an evacuation, a template version for a rollout";
            return Err(AppError::invalid("scope", why));
        }
        cmd.policy.validate().map_err(|e| AppError::invalid(e.field(), e.to_string()))?;
        let (permit, object) = self.may_decide(caller, cmd.kind, cmd.scope).await?;
        self.proposable(cmd.kind, cmd.scope).await?;
        let op = Operation {
            id: Uuid::now_v7(),
            kind: cmd.kind,
            scope: cmd.scope,
            policy: cmd.policy,
            status: Status::Proposed,
            reason: String::new(),
            proposed_by: caller.principal.clone(),
            proposed_under: permit.grant,
            proposed_at: self.clock.now(),
            approved: None,
            ended_at: None,
            version: 0,
        };
        let mut tx = self.store.begin().await?;
        let items = self.items_of(&mut tx, &op).await?;
        tx.insert_operation(&op, &items).await.map_err(node_busy)?;
        let mut e = caller.effect("operation.propose", Outcome::Succeeded).under(permit.grant);
        e = e.about(object).after(details([
            ("operation", op.id.to_string().into()),
            ("kind", op.kind.as_str().into()),
            ("items", items.len().into()),
        ]));
        if let Scope::Node(n) = op.scope {
            e.node = Some(n);
        }
        tx.append_effect(&e).await?;
        finish(tx, caller, idem, OPERATION, (op, items)).await
    }
}
