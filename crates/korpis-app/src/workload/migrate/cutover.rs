// SPDX-License-Identifier: AGPL-3.0-or-later

//! A migration's cutover (§8.1 of docs/05-scheduling.md): once nobody can be acting under the
//! source's epoch, released or past its grantor's end, the authoritative placement moves to the
//! destination in one transaction, each volume with it, onto the copy the destination reported
//! holding, or, for a volume nothing is sent for, the one both reach. The destination is then
//! granted the next epoch when it asks, by the ordinary lease rule (§4.5 of
//! docs/02-architecture.md), since it is where the workload is placed. Tried when the source's
//! journal says it released the lease, and every heartbeat interval for a lease that ran out,
//! by the sweep that also ends a migration one of whose nodes is not heard.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite as _, LeaseWrite as _, MigrationMoves as _, MigrationWrite as _,
    PlacementWrite as _, RecipeSource, Store, Tx as _, WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::node::Connectivity;
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::migration::{Migration, Next, Phase, Side};
use uuid::Uuid;

use super::conclude::EndedBy;

/// What the sweep does with a migration whose side `silent` names is not heard (§8.1 of
/// docs/05-scheduling.md), if anything: before the placement moved, a silent destination, or a
/// silent source before `cutover`, abandons it; a source silent at `cutover` is waited out, its
/// lease's end being what the cutover needs; a destination silent at `cutover` after the move
/// did not start the workload, and rolls it back. In `release` nothing: the workload runs on the
/// destination, and the source deletes its copies once it is heard again.
fn silenced(m: &Migration, silent: Side) -> Option<Next> {
    let why = format!("the {} node is unreachable", silent.as_str());
    match (m.moved_at.is_some(), silent) {
        (false, Side::Destination) => Some(Next::Abandon(why)),
        (false, Side::Source) if m.phase < Phase::Cutover => Some(Next::Abandon(why)),
        (true, Side::Destination) if m.phase == Phase::Cutover => Some(Next::RollBack(why)),
        _ => None,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// One pass over every migration that has not ended, each in a transaction of its own:
    /// one with a side not heard ends as `silenced` says, one in `cutover` whose source's lease
    /// is over moves, and one whose pass's ticket ran out starts it again. Returns how many it
    /// moved, ended, or started a pass of.
    pub async fn tend_migrations(&self) -> Result<u64, AppError> {
        let mut tended = 0;
        for m in self.store.live_migrations().await? {
            let silent = self.silent_side(&m).await?;
            let mut tx = self.store.begin().await?;
            let done = match silent.and_then(|side| silenced(&m, side)) {
                Some(then) => self.end_silenced(&mut tx, m.id, then).await?,
                None => {
                    self.cut_over_in(&mut tx, m.workload).await?
                        || self.renew_passes(&mut tx, m.id).await?
                }
            };
            if done {
                tx.commit().await?;
                tended += 1;
            }
        }
        Ok(tended)
    }

    /// Starts in `tx` each pass of the migration `id` whose ticket ran out before the destination
    /// held it; whether any.
    async fn renew_passes(&self, tx: &mut S::Tx, id: Uuid) -> Result<bool, AppError> {
        let Some((read, _)) = self.store.migration(id).await? else { return Ok(false) };
        if !matches!(read.phase, Phase::Replicate | Phase::FinalSync) || !read.live() {
            return Ok(false);
        }
        tx.lock_workload(read.workload).await?;
        let Some(m) = tx.migration_in(id).await? else { return Ok(false) };
        Ok(m.live() && self.start_due_passes(tx, &m).await? > 0)
    }

    /// The side of `m` whose node is unreachable, the destination first, if either.
    async fn silent_side(&self, m: &Migration) -> Result<Option<Side>, AppError> {
        for (side, node) in [(Side::Destination, m.destination), (Side::Source, m.source)] {
            let heard = self.store.node(node).await?.map(|n| n.connectivity);
            if heard == Some(Connectivity::Unreachable) {
                return Ok(Some(side));
            }
        }
        Ok(None)
    }

    /// Does `then` to the migration `id` in `tx`, if it is still as it was read; whether it did.
    async fn end_silenced(&self, tx: &mut S::Tx, id: Uuid, then: Next) -> Result<bool, AppError> {
        let Some((read, _)) = self.store.migration(id).await? else { return Ok(false) };
        tx.lock_workload(read.workload).await?;
        let Some(m) = tx.migration_in(id).await? else { return Ok(false) };
        if m != read {
            return Ok(false);
        }
        self.follow(tx, &m, then).await?;
        Ok(true)
    }

    /// Moves the placement of `workload` in `tx`, if a migration of it is in `cutover`, has not
    /// moved, and nobody can be acting under its source's epoch; whether it moved, or rolled
    /// back where the destination reported no copy of a volume sent.
    pub(crate) async fn cut_over_in(
        &self,
        tx: &mut S::Tx,
        workload: Uuid,
    ) -> Result<bool, AppError> {
        let Some(w) = tx.lock_workload(workload).await? else { return Ok(false) };
        let Some(m) = tx.live_migration_in(workload).await? else { return Ok(false) };
        if m.phase != Phase::Cutover || m.moved_at.is_some() {
            return Ok(false);
        }
        let now = self.clock.now();
        let lease = tx.lease_for_update(workload).await?;
        if lease.as_ref().is_some_and(|l| l.node == m.source && !l.over(now)) {
            return Ok(false);
        }
        let mut volumes = Vec::new();
        for v in tx.migration_volumes_in(m.id).await? {
            match (v.crossing.sends(), v.destination_helper_id) {
                (_, Some(helper)) => volumes.push((v.volume, helper)),
                (false, None) => volumes.push((v.volume, v.source_helper_id)),
                (true, None) => {
                    let why = format!("the destination reported no copy of volume {}", v.name);
                    self.roll_back_in(tx, &m, (&why, EndedBy::Failure)).await?;
                    return Ok(true);
                }
            }
        }
        tx.move_placement((m.id, workload), (m.source, m.destination), &volumes, now).await?;
        let mut e = moved(&m, volumes.len());
        e.grant = tx.decision_grant(workload).await?;
        tx.append_effect(&e).await?;
        self.count_placed(tx, &w).await?;
        Ok(true)
    }
}

/// The effect of moving `m`'s placement with `volumes` volumes.
fn moved(m: &Migration, volumes: usize) -> Effect {
    let mut e = Effect::new(
        Principal::System(SystemActor::Scheduler),
        "migration.cutover",
        Outcome::Succeeded,
    )
    .after(details([
        ("migration", m.id.to_string().into()),
        ("source", m.source.to_string().into()),
        ("destination", m.destination.to_string().into()),
        ("volumes", volumes.into()),
    ]));
    (e.workload, e.plan, e.node) = (Some(m.workload), Some(m.plan), Some(m.destination));
    e
}
