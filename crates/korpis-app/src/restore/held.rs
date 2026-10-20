// SPDX-License-Identifier: AGPL-3.0-or-later

//! Confirming a placement a restore held (Finding 97 of docs/23-walkthroughs.md): its node
//! presented its assignments without it, which a retirement after the backup explains and so
//! does an assignment never delivered, and only an operator can say which. Confirming sends it to
//! its node again; retiring it is the other answer, and needs nothing here.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite as _, PlacementWrite as _, RecipeSource, RecoveryWrite as _, Store,
    WorkloadWrite as _,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::placement::Placement;
use uuid::Uuid;

const OPERATION: &str = "workload.confirm_placement";

/// The longest reason, in characters.
const MAX_REASON: usize = 500;

/// Confirming a held placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmPlacement {
    /// The workload.
    pub workload: Uuid,
    /// Why, for the record.
    pub reason: String,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Confirms the held placement of a workload: `node.place` on its node and `workload.write`
    /// on the workload, as placing it by hand needs. Refused for a placement not held.
    pub async fn confirm_placement(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Placement>,
        cmd: ConfirmPlacement,
    ) -> Result<Reply<Placement>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let reason = cmd.reason.trim();
        if reason.is_empty() || reason.chars().count() > MAX_REASON {
            return Err(AppError::invalid("reason", "why, in at most 500 characters"));
        }
        let object = ObjectRef::new(Kind::Workload, cmd.workload);
        let placed = self.store.placement(cmd.workload).await?;
        let placed = placed.ok_or(AppError::NotFound { kind: "placement" })?;
        let permit =
            self.authorize(caller, "node.place", ObjectRef::new(Kind::Node, placed.node)).await?;
        self.authorize(caller, "workload.write", object).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_workload(cmd.workload).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        let Some(node) = tx.confirm_held(cmd.workload).await? else {
            return Err(AppError::conflict("placement.not_held", "the placement is not held"));
        };
        let placement = tx.placement_in(cmd.workload).await?;
        let placement = placement.ok_or(AppError::NotFound { kind: "placement" })?;
        let mut e = caller
            .effect("workload.placement.confirm", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([("node", node.to_string().into()), ("reason", reason.into())]));
        e.workload = Some(cmd.workload);
        e.node = Some(node);
        tx.append_effect(&e).await?;
        finish(tx, caller, idem, OPERATION, placement).await
    }
}
