// SPDX-License-Identifier: AGPL-3.0-or-later

//! A live migration's state, on both nodes. The source makes the state volume its role names before
//! the first pass, so each pass before `quiesce` sends an empty directory, and at `quiesce` has its
//! driver checkpoint the running workload into it, which leaves the object stopped, as the
//! reconciler is told it is from then on; `final_sync` carries the state. The destination, as it
//! makes the workload at `cutover`, resumes it from its copy instead of starting it, and deletes the
//! copy once it runs. A checkpoint that fails fails `quiesce`, which abandons the migration with the
//! workload where it was; a state that does not resume fails `cutover`, which rolls it back, and is
//! never booted in its place. A state volume left on the source by a migration that ended without
//! its release is deleted, since it holds the guest's memory.

use super::super::act::{Failure, running, volume_id};
use super::super::volume::Why;
use super::super::{Reconciler, Tracked};
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_core::workload::volume::LIVE_STATE;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;

/// The state volume `role` names, if its migration is live.
fn state_of(role: &agent::MigrationRole) -> Option<&agent::MigrationVolumeRole> {
    role.volumes.iter().find(|v| role.live && v.name == LIVE_STATE)
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// The state volume `v` of `a`'s workload, known to the helper as `helper_id`, made if it is
    /// not; its path.
    async fn state_volume(
        &self,
        a: &agent::Assignment,
        v: &agent::MigrationVolumeRole,
        helper_id: String,
    ) -> Result<String, Failure> {
        let request = helper::EnsureVolumeRequest {
            volume: helper_id,
            workload: a.workload.clone(),
            storage_class: v.storage_class.clone(),
            ..Default::default()
        };
        Ok(self.ensure_volume(LIVE_STATE, Why::State, &request).await?.host_path)
    }

    /// On a live migration's source, up to `quiesce`: the state volume made, once a migration,
    /// and at `quiesce` the running workload checkpointed into it.
    pub(super) async fn capture(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        role: &agent::MigrationRole,
    ) {
        let Some(v) = state_of(role) else { return };
        if t.migrated.state.as_ref().is_none_or(|(m, _)| *m != role.migration) {
            // Kept before it is made, so a restart between the two still deletes it.
            let kept = self.progress.stating(&a.workload, Some(v.source_helper_id.clone()));
            if let Err(e) = kept {
                return tracing::error!(error = %e, "progress could not be kept");
            }
            match self.state_volume(a, v, v.source_helper_id.clone()).await {
                Ok(dir) => t.migrated.state = Some((role.migration.clone(), dir)),
                Err(e) => return tracing::warn!(workload = %a.workload, error = %e.0, "no state"),
            }
        }
        let quiesce = role.phase.as_known() == Some(agent::MigrationPhase::MIGRATION_PHASE_QUIESCE);
        let Some(((_, dir), id)) = t.migrated.state.clone().zip(t.runtime.clone()) else { return };
        if !quiesce || !t.status.as_ref().is_some_and(running) {
            return;
        }
        match self.runtime.checkpoint(&id, &dir).await {
            Ok(_) => {
                (t.stopped_by_us, t.running_since) = (true, None);
                t.status = self.runtime.inspect(&id).await.ok();
            }
            Err(e) => {
                let why = format!("the workload's state could not be captured: {e}");
                self.report(t, role, (Vec::new(), Vec::new()), Some(why)).await;
            }
        }
    }

    /// On a live migration's destination, once its copies are the workload's at `cutover`: where
    /// its copy of the state is, which the workload is resumed from as it is made.
    pub(super) async fn to_resume(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        role: &agent::MigrationRole,
    ) -> Result<(), Failure> {
        let Some(v) = state_of(role) else { return Ok(()) };
        let dir = self.state_volume(a, v, volume_id(a, LIVE_STATE)).await?;
        t.migrated.resume = Some((role.migration.clone(), dir));
        Ok(())
    }

    /// Starts object `id` of `a`'s workload: from the state its live migration carried here, if
    /// one is to be resumed, and the state deleted once it runs or once it did not resume;
    /// otherwise afresh.
    pub(in crate::reconcile) async fn start_or_resume(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        id: &str,
    ) -> Result<(), Failure> {
        let role = a.migration.as_option();
        let Some((migration, dir)) = t.migrated.resume.clone() else {
            return Ok(self.runtime.start(id).await?);
        };
        let Some(role) = role.filter(|r| r.migration == migration) else {
            // The migration it was carried by ended: what is left of it is not resumed.
            t.migrated.resume = None;
            return Ok(self.runtime.start(id).await?);
        };
        let resumed = self.runtime.resume(id, &dir).await;
        let helper_id = volume_id(a, LIVE_STATE);
        if let Err(e) = self.delete_volume(&a.workload, (LIVE_STATE, Why::State), &helper_id).await
        {
            tracing::warn!(workload = %a.workload, error = %e.0, "a resumed state not deleted");
        }
        match resumed {
            Ok(()) => {
                t.migrated.resume = None;
                Ok(())
            }
            Err(e) => {
                let why = format!("the workload's state did not resume: {e}");
                self.report(t, role, (Vec::new(), Vec::new()), Some(why.clone())).await;
                Err(Failure(why))
            }
        }
    }

    /// Deletes the state volume a live migration left on this node as its source, under the
    /// identifier it was made with, once the workload is held here under no migration: what this
    /// node keeps of it outlives a restart.
    pub(super) async fn forget_state(&mut self, a: &agent::Assignment, t: &mut Tracked) {
        let Some(helper_id) = self.progress.of(&a.workload).state else { return };
        match self.delete_volume(&a.workload, (LIVE_STATE, Why::State), &helper_id).await {
            Ok(()) => {
                t.migrated.state = None;
                if let Err(e) = self.progress.stating(&a.workload, None) {
                    tracing::error!(error = %e, "progress could not be kept");
                }
            }
            Err(e) => tracing::warn!(state = %helper_id, error = %e.0, "a left state not deleted"),
        }
    }

    /// Deletes the state volume kept here for each workload no longer held here: its migration
    /// moved it away, and its release may not have deleted the state before it left.
    pub(super) async fn forget_states(&mut self, held: &crate::held::HeldSet) {
        for (workload, helper_id) in self.progress.states() {
            if held.assignments.contains_key(&workload) {
                continue;
            }
            if let Err(e) = self.host.delete_volume(&helper_id).await {
                tracing::warn!(state = %helper_id, error = %e.0, "a left state not deleted");
                continue;
            }
            if let Err(e) = self.progress.stating(&workload, None) {
                tracing::error!(error = %e, "progress could not be kept");
            }
        }
    }
}
