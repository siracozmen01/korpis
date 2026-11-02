// SPDX-License-Identifier: AGPL-3.0-or-later

//! An evacuation's item: its workload placed again from its target off the node, with each volume
//! from where the evacuation says, recorded on the item as it begins, since what it found when it
//! was proposed may have changed: a newer backup, a replica that caught up, or one dropped.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use crate::workload::Evacuating;
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::operation::{Item, ItemState, Operation, Scope};

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Begins `op`'s `item`, in `tx`, for `approver`; a workload whose lease has not ended stays
    /// waiting.
    pub(super) async fn evacuate_item(
        &self,
        tx: &mut S::Tx,
        op: &Operation,
        approver: &Caller,
        item: &mut Item,
    ) -> Result<(), AppError> {
        let Scope::Node(node) = op.scope else { return Ok(()) };
        let object = ObjectRef::new(Kind::Node, node);
        let permit = self.authorize(approver, "node.evacuate", object).await?;
        let now = self.clock.now();
        let detail = self.evacuation(item.subject, node).await?.detail();
        let by = (op.id, approver, permit.grant);
        match self.evacuate_for(tx, by, (item.subject, node)).await? {
            Evacuating::Gone => item.end(ItemState::Skipped, "no longer on the node", now),
            Evacuating::NotYet => {}
            Evacuating::Placed(plan) => {
                item.state = ItemState::Applying;
                item.plan = plan;
                item.detail = detail;
                item.begun_at = Some(now);
            }
        }
        Ok(())
    }
}
