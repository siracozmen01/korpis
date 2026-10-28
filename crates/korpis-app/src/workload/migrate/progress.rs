// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's report of its side of a migration's phase (§8.1 of docs/05-scheduling.md), and what
//! the control plane does with it. A report is believed only from the node assigned that side,
//! about the phase the migration is in; anything else is `journal.rejected`. Once every side
//! acting in the phase reported it done, both nodes move on; a phase failed ends the migration,
//! abandoned before the placement moved and rolled back from then, and the source's deletion
//! of its copies ends it completed. Each end is an effect of its own.

use crate::App;
use crate::error::AppError;
use crate::node::journal::rejected;
use crate::ports::{
    Clock, EffectWrite as _, MigrationMoves as _, MigrationWrite as _, RecipeSource, Store,
    WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::migration::{
    Migration, Next, Phase, Progress, Side, Standing, failed, next,
};
use uuid::Uuid;

use super::conclude::EndedBy;

const ACTION: &str = "migration.progress";

/// Why a report from `node` about `m` is not believed, if it is not.
fn disbelieved(m: &Migration, node: Uuid, p: &Progress) -> Option<&'static str> {
    if !m.live() {
        Some("the migration ended")
    } else if Side::of(m, node) != Some(p.side) {
        Some("the node is not that side of the migration")
    } else if p.phase != m.phase {
        Some("a report of a phase the migration is not in")
    } else {
        None
    }
}

/// An effect of the control plane about `m`.
fn about(m: &Migration, action: &str, d: [(&str, serde_json::Value); 3]) -> Effect {
    let mut e = Effect::new(Principal::System(SystemActor::Reconciler), action, Outcome::Succeeded)
        .after(details(d));
    (e.workload, e.plan) = (Some(m.workload), Some(m.plan));
    e
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Records `node`'s report `p` in `tx` and does what follows; the effect recording it.
    pub(crate) async fn migration_progress(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        p: &Progress,
    ) -> Result<Effect, AppError> {
        // The workload first, then the migration, in the order every other writer takes them.
        let Some((found, _)) = self.store.migration(p.migration).await? else {
            return Ok(rejected("no such migration", ACTION));
        };
        tx.lock_workload(found.workload).await?;
        let Some(m) = tx.migration_in(found.id).await? else {
            return Ok(rejected("no such migration", ACTION));
        };
        if let Some(why) = disbelieved(&m, node, p) {
            return Ok(rejected(why, ACTION));
        }
        tx.record_volume_progress(m.id, p.side, &p.volumes).await?;
        if p.side == Side::Source && p.phase == Phase::Quiesce {
            tx.record_watermarks(m.id, &p.watermarks).await?;
        }
        let then = match p.standing {
            Standing::Going => Next::Wait,
            Standing::Failed => failed(&m, p.phase, &p.detail),
            Standing::Done => {
                tx.record_reported(m.id, p.side, p.phase).await?;
                let mut reported = m.reported;
                match p.side {
                    Side::Source => reported.source = Some(p.phase),
                    Side::Destination => reported.destination = Some(p.phase),
                }
                next(&m, reported, &tx.migration_volumes_in(m.id).await?)
            }
        };
        self.follow(tx, &m, then).await?;
        Ok(about(
            &m,
            ACTION,
            [
                ("migration", m.id.to_string().into()),
                ("phase", p.phase.as_str().into()),
                ("side", p.side.as_str().into()),
            ],
        ))
    }

    /// Carries out `then` for `m` in `tx`, recording an end as an effect of its own.
    pub(super) async fn follow(
        &self,
        tx: &mut S::Tx,
        m: &Migration,
        then: Next,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let (action, why) = match then {
            Next::Wait => return Ok(()),
            Next::Advance(phase) => {
                let hold_until = (phase == Phase::Release).then(|| now + m.hold);
                tx.advance_migration(m.id, phase, (now, hold_until)).await?;
                return Ok(());
            }
            Next::Complete => {
                self.complete_migration(tx, m).await?;
                ("migration.complete", String::new())
            }
            Next::Abandon(why) => {
                self.abandon_in(tx, m, (&why, EndedBy::Failure)).await?;
                ("migration.abandon", why)
            }
            Next::RollBack(why) => {
                self.roll_back_in(tx, m, (&why, EndedBy::Failure)).await?;
                ("migration.rollback", why)
            }
        };
        let e = about(
            m,
            action,
            [
                ("migration", m.id.to_string().into()),
                ("phase", m.phase.as_str().into()),
                ("reason", why.into()),
            ],
        );
        tx.append_effect(&e).await?;
        Ok(())
    }
}
