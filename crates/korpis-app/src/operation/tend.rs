// SPDX-License-Identifier: AGPL-3.0-or-later

//! The operation reconciler: on every pass of the control plane's loop, each operation approved
//! and not ended starts once its time has come, has each item under way settled against what the
//! nodes report, ends once every item has, and then begins as many waiting items as its policy
//! admits, each in a transaction of its own under the operation's lock, so a pause or an abort
//! decided meanwhile is seen before the next item begins. An item that cannot begin fails alone,
//! for the reason its refusal gives, and its operation pauses, goes on, or fails as its policy
//! says; a fault of the control plane's own fails nothing, and the item is tried at the next pass.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, EffectWrite as _, OperationWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::operation::{Admission, Item, ItemState, Kind, Operation, Progress, Status};
use korpis_core::principal::{Principal, SystemActor};
use uuid::Uuid;

/// Whether `e` is a refusal of the item, which fails it, rather than a fault, which is retried.
const fn refuses(e: &AppError) -> bool {
    !matches!(
        e,
        AppError::Internal(_) | AppError::Unavailable { .. } | AppError::RateLimited { .. }
    )
}

/// The effect of `op`'s item `item` ending as it did.
fn item_effect(op: &Operation, item: &Item) -> Effect {
    let actor = Principal::System(SystemActor::Scheduler);
    let mut e = Effect::new(actor, "operation.item", Outcome::Succeeded).after(details([
        ("operation", op.id.to_string().into()),
        ("subject", item.subject.to_string().into()),
        ("state", item.state.as_str().into()),
        ("reason", item.reason.clone().into()),
    ]));
    if item.state == ItemState::Failed {
        e.outcome = Outcome::Failed;
    }
    e.grant = op.approved.as_ref().map(|a| a.under);
    e.plan = item.plan;
    e
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One pass over every operation approved and not ended. Returns how many items began or
    /// ended.
    pub async fn tend_operations(&self) -> Result<u64, AppError> {
        let mut moved = 0;
        for id in self.store.live_operations().await? {
            moved += self.tend_operation(id).await?;
        }
        Ok(moved)
    }

    /// Starts, settles, and ends operation `id`, then begins what its policy admits.
    async fn tend_operation(&self, id: Uuid) -> Result<u64, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Some((mut op, mut items)) = tx.operation_in(id).await? else { return Ok(0) };
        let (read_at, was) = (op.version, op.status);
        let started = op.start(now);
        let mut moved = 0;
        let under_way =
            |i: &&mut Item| matches!(i.state, ItemState::Applying | ItemState::Settling);
        for item in items.iter_mut().filter(under_way) {
            moved += u64::from(self.settled(&mut tx, &mut op, item, now).await?);
        }
        let progress = Progress::of(&items);
        if op.conclude(&progress, now) || op.status.ended() {
            self.release_node(&mut tx, &op).await?;
        }
        if started || moved > 0 || op.status != was {
            tx.update_operation(&op, read_at).await?;
        }
        tx.commit().await?;
        if op.status != Status::Running {
            return Ok(moved);
        }
        let admitted = Admission::under(&op.policy, &progress).0;
        let waiting: Vec<Uuid> =
            items.iter().filter(|i| i.state == ItemState::Waiting).map(|i| i.subject).collect();
        for subject in waiting.into_iter().take(usize::try_from(admitted).unwrap_or(usize::MAX)) {
            moved += u64::from(self.begin_item(id, subject).await?);
        }
        Ok(moved)
    }

    /// Settles `item`, under way in `op`, at `now`, writing it if it moved and doing what `op`'s
    /// policy says if it failed; whether it moved.
    async fn settled(
        &self,
        tx: &mut S::Tx,
        op: &mut Operation,
        item: &mut Item,
        now: std::time::SystemTime,
    ) -> Result<bool, AppError> {
        let was = item.state;
        self.settle(op, item, now).await?;
        if item.state == was {
            return Ok(false);
        }
        tx.put_item(item).await?;
        if item.state.ended() {
            tx.append_effect(&item_effect(op, item)).await?;
        }
        if item.state == ItemState::Failed {
            op.item_failed(&item.reason, now);
        }
        Ok(true)
    }

    /// Begins `op`'s waiting item about `subject`, if `op` still runs; whether it began or ended.
    async fn begin_item(&self, id: Uuid, subject: Uuid) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let Some((op, items)) = tx.operation_in(id).await? else { return Ok(false) };
        let Some(mut item) = items.into_iter().find(|i| i.subject == subject) else {
            return Ok(false);
        };
        if op.status != Status::Running || item.state != ItemState::Waiting {
            return Ok(false);
        }
        let Some(approval) = op.approved.clone() else { return Ok(false) };
        let approver = Caller::new(approval.by.clone(), None, approval.mfa);
        let begun = match op.kind {
            Kind::Drain => self.drain_item(&mut tx, &op, &approver, &mut item).await,
            Kind::Evacuate => self.evacuate_item(&mut tx, &op, &approver, &mut item).await,
            Kind::TemplateRollout => self.rollout_item(&mut tx, &op, &approver, &mut item).await,
        };
        match begun {
            Ok(()) => {
                tx.put_item(&item).await?;
                if item.state.ended() {
                    tx.append_effect(&item_effect(&op, &item)).await?;
                }
                tx.commit().await?;
                Ok(true)
            }
            Err(e) if refuses(&e) => {
                drop(tx);
                self.fail_item(id, subject, &e.to_string()).await
            }
            Err(e) => Err(e),
        }
    }

    /// Fails `op`'s item about `subject`, for `why`, and does what `op`'s policy says.
    async fn fail_item(&self, id: Uuid, subject: Uuid, why: &str) -> Result<bool, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let Some((mut op, items)) = tx.operation_in(id).await? else { return Ok(false) };
        let Some(mut item) = items.into_iter().find(|i| i.subject == subject) else {
            return Ok(false);
        };
        let read_at = op.version;
        item.end(ItemState::Failed, why, now);
        op.item_failed(why, now);
        if op.status.ended() {
            self.release_node(&mut tx, &op).await?;
        }
        tx.put_item(&item).await?;
        tx.append_effect(&item_effect(&op, &item)).await?;
        tx.update_operation(&op, read_at).await?;
        tx.commit().await?;
        Ok(true)
    }
}
