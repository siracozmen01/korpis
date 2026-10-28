// SPDX-License-Identifier: AGPL-3.0-or-later

//! Starting a migration's passes (§8.1 of docs/05-scheduling.md): the first of `replicate` and
//! the one of `final_sync` as the phase is entered, the next of `replicate` once the one under
//! way is held and was large, and one whose ticket ran out again. The control plane names each,
//! so both nodes' roles carry its ticket and both report it by that name.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, MigrationMoves as _, RecipeSource, Store};
use korpis_core::workload::migration::PASS_LIFETIME as LIFETIME;
use korpis_core::workload::migration::{Migration, Phase};
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Starts in `tx` each pass of `m`'s volumes that is due in its phase; how many.
    pub(super) async fn start_due_passes(
        &self,
        tx: &mut S::Tx,
        m: &Migration,
    ) -> Result<usize, AppError> {
        let now = self.clock.now();
        let mut started = 0;
        for v in tx.migration_volumes_in(m.id).await? {
            if let Some(p) = v.pass_due(m.phase, (now, LIFETIME), Uuid::now_v7()) {
                tx.start_pass(m.id, p, now).await?;
                started += 1;
            }
        }
        Ok(started)
    }

    /// Starts in `tx` the pass of each volume of the migration `id` that `phase`, just entered,
    /// sends.
    pub(super) async fn enter_passes(
        &self,
        tx: &mut S::Tx,
        id: Uuid,
        phase: Phase,
    ) -> Result<(), AppError> {
        if !matches!(phase, Phase::Replicate | Phase::FinalSync) {
            return Ok(());
        }
        let now = self.clock.now();
        for v in tx.migration_volumes_in(id).await? {
            if let Some(p) = v.entry_pass(Uuid::now_v7()) {
                tx.start_pass(id, p, now).await?;
            }
        }
        Ok(())
    }
}
