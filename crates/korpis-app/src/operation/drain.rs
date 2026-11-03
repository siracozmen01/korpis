// SPDX-License-Identifier: AGPL-3.0-or-later

//! A drain's item: its workload moved off the node by a migration toward the scheduler's choice,
//! as any migration moves it, the node taking part. One deleted, or moved off the node meanwhile,
//! is skipped; one the approver may no longer drain the node for begins nothing.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, PlacementWrite as _, RecipeSource, Store, WorkloadWrite as _};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::operation::{Item, ItemState, Operation, Scope};

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Begins `op`'s `item`, in `tx`, for `approver`.
    pub(super) async fn drain_item(
        &self,
        tx: &mut S::Tx,
        op: &Operation,
        approver: &Caller,
        item: &mut Item,
    ) -> Result<(), AppError> {
        let Scope::Node(node) = op.scope else { return Ok(()) };
        let permit =
            self.authorize(approver, "node.drain", ObjectRef::new(Kind::Node, node)).await?;
        let now = self.clock.now();
        let w = tx.lock_workload(item.subject).await?.filter(|w| w.deleted_at.is_none());
        let on_node = match &w {
            Some(w) => tx.placement_in(w.id).await?.is_some_and(|p| p.node == node),
            None => false,
        };
        let Some(w) = w.filter(|_| on_node) else {
            item.end(ItemState::Skipped, "no longer on the node", now);
            return Ok(());
        };
        let plan = self.migrate_for(tx, (op.id, approver, permit.grant), &w).await?;
        item.state = ItemState::Applying;
        item.plan = Some(plan.id);
        item.begun_at = Some(now);
        Ok(())
    }
}
