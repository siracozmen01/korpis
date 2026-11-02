// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's part in a migration of a workload it holds, beside the copies (`migrate`). The
//! control plane shapes both assignments: the source is told the workload stopped from `quiesce`
//! on, which the reconciler does under its lease, and the side the workload is not placed on is a
//! standby, with no plan and no lease. What is left is each side's report and what nothing else
//! does. The destination prepares the target and fetches its content at `prepare`, keeping which
//! of its volumes hold copies; the source reports `quiesce` once the workload is stopped and its
//! streams sealed, with each stream's watermark; at `cutover` the source releases its lease and
//! the destination, once granted the next, promotes each copy received by increments into the
//! volume before the workload is made, and reports once it runs; at `release`, once the hold and
//! a margin for the two clocks have passed, the source deletes the workload's object and its
//! copies. A migration abandoned before the workload moved leaves the destination's copies
//! unclaimed, and they are deleted.

use super::act::{Failure, running, volume_id};
use super::{Reconciler, Tracked};
use crate::host::Host;
use crate::lease::Standing;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

mod streams;

/// How long after the hold's end the source waits before deleting its copies: the control plane
/// refuses a rollback from the hold's end by its own clock, which this node's may lag.
const SKEW: Duration = Duration::from_secs(60);

/// What this node did of a migration of the workload, so each report is journaled once.
#[derive(Debug, Clone, Default)]
pub(super) struct Migrated {
    /// The migration, and the last phase this node reported of it.
    reported: Option<(String, agent::MigrationPhase)>,
    /// The migration whose copies this node promoted into the workload's volumes.
    promoted: Option<String>,
}

/// The phase `role` names, and the side this node is.
fn standing_of(role: &agent::MigrationRole) -> (Option<agent::MigrationSide>, Option<agent::MigrationPhase>) {
    (role.role.as_known(), role.phase.as_known())
}

/// Whether `v` crosses by increments.
fn by_increments(v: &agent::MigrationVolumeRole) -> bool {
    v.crossing.as_known() == Some(pb::MigrationCrossing::MIGRATION_CROSSING_INCREMENTS)
}

/// Whether `v` crosses at all: a local volume, which the source holds a copy of.
fn crosses(v: &agent::MigrationVolumeRole) -> bool {
    !matches!(
        v.crossing.as_known(),
        Some(pb::MigrationCrossing::MIGRATION_CROSSING_NOTHING) | None
    )
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// The node's part in `a`'s migration before the lease decides what the reconciler does;
    /// whether it acts on `a` as usual.
    pub(super) async fn migrating_before(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
        standing: &Standing,
    ) -> bool {
        use agent::MigrationPhase as P;
        use agent::MigrationSide as S;
        let Some(role) = a.migration.as_option() else { return true };
        match standing_of(role) {
            (Some(S::MIGRATION_SIDE_DESTINATION), Some(P::MIGRATION_PHASE_PREPARE)) => {
                self.prepared(a, t, role).await;
                true
            }
            (Some(S::MIGRATION_SIDE_DESTINATION), Some(P::MIGRATION_PHASE_CUTOVER | P::MIGRATION_PHASE_RELEASE))
                if matches!(standing, Standing::Held { .. }) =>
            {
                self.promoted(a, t, role).await
            }
            (Some(S::MIGRATION_SIDE_SOURCE), Some(P::MIGRATION_PHASE_CUTOVER)) => {
                if let Err(e) = self.wiring.leases.release(&a.workload).await {
                    tracing::error!(error = %e, "the release could not be journalled");
                }
                false
            }
            (Some(S::MIGRATION_SIDE_SOURCE), Some(P::MIGRATION_PHASE_RELEASE)) => {
                self.let_go(a, t, role).await;
                false
            }
            _ => true,
        }
    }

    /// The node's part in `a`'s migration once the reconciler acted: the source reports
    /// `quiesce` once the workload stopped, and the destination `cutover` once it runs.
    pub(super) async fn migrating_after(&mut self, a: &agent::Assignment, t: &mut Tracked) {
        use agent::MigrationPhase as P;
        use agent::MigrationSide as S;
        let Some(role) = a.migration.as_option() else { return };
        match standing_of(role) {
            (Some(S::MIGRATION_SIDE_SOURCE), Some(P::MIGRATION_PHASE_QUIESCE)) => {
                let stopped = !t.status.as_ref().is_some_and(running);
                if stopped && let Some(watermarks) = self.watermarks(&a.workload) {
                    self.report(t, role, (Vec::new(), watermarks), None).await;
                }
            }
            (Some(S::MIGRATION_SIDE_DESTINATION), Some(P::MIGRATION_PHASE_CUTOVER)) => {
                if t.status.as_ref().is_some_and(running) {
                    self.report(t, role, (Vec::new(), Vec::new()), None).await;
                }
            }
            _ => {}
        }
    }

    /// At `prepare`, on the destination: the target prepared and its content fetched, and the
    /// volumes copies are received into kept, reported once.
    async fn prepared(&mut self, a: &agent::Assignment, t: &mut Tracked, role: &agent::MigrationRole) {
        if self.reported(t, role) {
            return;
        }
        let copies: Vec<String> = role
            .volumes
            .iter()
            .filter(|v| crosses(v))
            .map(|v| volume_id(a, &v.name))
            .collect();
        if let Err(e) = self.progress.receiving(&a.workload, copies) {
            tracing::error!(error = %e, "progress could not be kept");
            return;
        }
        let fetched = match self.prepare(a, t).await {
            Ok(spec) => self.host.fetch(&spec.rootfs).await.map_err(Failure::from),
            Err(e) => Err(e),
        };
        let volumes = role
            .volumes
            .iter()
            .filter(|v| crosses(v))
            .map(|v| agent::MigrationVolumeProgress {
                name: v.name.clone(),
                helper_id: volume_id(a, &v.name),
                ..Default::default()
            })
            .collect();
        self.report(t, role, (volumes, Vec::new()), fetched.err().map(|e| e.0)).await;
    }

    /// At `cutover` and after, on the destination holding the lease: each copy received by
    /// increments promoted into the volume, so the workload is made over what crossed; whether it
    /// may be made. A copy that cannot be promoted fails the phase, which rolls the workload back.
    async fn promoted(&mut self, a: &agent::Assignment, t: &mut Tracked, role: &agent::MigrationRole) -> bool {
        if t.migrated.promoted.as_ref() == Some(&role.migration) {
            return true;
        }
        for v in role.volumes.iter().filter(|v| by_increments(v) && !v.held_pass.is_empty()) {
            let request = helper::PromoteReplicaRequest {
                volume: v.source_helper_id.clone(),
                storage_class: crate::migrate::class_of(a, &v.name),
                snapshot: v.held_pass.clone(),
                into: volume_id(a, &v.name),
                own_from: crate::migrate::own_snapshots_of(&v.source_helper_id),
                ..Default::default()
            };
            if let Err(e) = self.host.promote_replica(&request).await {
                let why = format!("volume {} could not be promoted: {}", v.name, e.0);
                self.report(t, role, (Vec::new(), Vec::new()), Some(why)).await;
                return false;
            }
        }
        // The copies are the workload's volumes now: never deleted as an abandoned migration's.
        if let Err(e) = self.progress.receiving(&a.workload, Vec::new()) {
            tracing::error!(error = %e, "progress could not be kept");
            return false;
        }
        t.migrated.promoted = Some(role.migration.clone());
        true
    }

    /// At `release`, on the source once the hold has passed: the workload's object and each
    /// local copy deleted, reported once; what fails is tried again at the next pass.
    async fn let_go(&mut self, a: &agent::Assignment, t: &mut Tracked, role: &agent::MigrationRole) {
        let hold = role.hold_until.as_option().and_then(|h| korpis_wire::time("hold_until", h).ok());
        let passed = hold.is_some_and(|h| h + SKEW <= SystemTime::now());
        if !passed || self.reported(t, role) {
            return;
        }
        match self.destroy(t).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => return tracing::warn!(workload = %a.workload, error = %e.0, "not destroyed"),
        }
        for v in role.volumes.iter().filter(|v| crosses(v)) {
            if let Err(e) = self.host.delete_volume(&v.source_helper_id).await {
                tracing::warn!(volume = %v.name, error = %e.0, "a migrated copy not deleted");
                return;
            }
        }
        self.report(t, role, (Vec::new(), Vec::new()), None).await;
    }

    /// Deletes the copies kept for each workload that left the set without moving here: its
    /// migration was abandoned before the cutover.
    pub(super) async fn abandoned(&mut self, held: &crate::held::HeldSet) {
        for (workload, volumes) in self.progress.received() {
            if held.assignments.contains_key(&workload) {
                continue;
            }
            let mut left = Vec::new();
            for v in volumes {
                if let Err(e) = self.host.delete_volume(&v).await {
                    tracing::warn!(volume = %v, error = %e.0, "an abandoned copy not deleted");
                    left.push(v);
                }
            }
            if let Err(e) = self.progress.receiving(&workload, left) {
                tracing::error!(error = %e, "progress could not be kept");
            }
        }
    }

    /// Whether this node already reported `role`'s phase.
    fn reported(&self, t: &Tracked, role: &agent::MigrationRole) -> bool {
        let phase = role.phase.as_known();
        t.migrated.reported.as_ref().is_some_and(|(m, p)| *m == role.migration && Some(*p) == phase)
    }

    /// Journals this node's report of `role`'s phase, with each volume's part and the streams'
    /// watermarks: done, or failed for `failed`; once per phase.
    async fn report(
        &self,
        t: &mut Tracked,
        role: &agent::MigrationRole,
        (volumes, watermarks): (Vec<agent::MigrationVolumeProgress>, Vec<agent::StreamWatermark>),
        failed: Option<String>,
    ) {
        use agent::MigrationOutcomeOfPhase as O;
        let Some(phase) = role.phase.as_known() else { return };
        if self.reported(t, role) {
            return;
        }
        let (outcome, detail) = match failed {
            Some(why) => (O::MIGRATION_OUTCOME_OF_PHASE_FAILED, why),
            None => (O::MIGRATION_OUTCOME_OF_PHASE_DONE, String::new()),
        };
        let p = agent::MigrationProgress {
            migration: role.migration.clone(),
            phase: role.phase,
            side: role.role,
            outcome: outcome.into(),
            detail,
            volumes,
            watermarks,
            ..Default::default()
        };
        match self.wiring.journal.append(Body::Migration(Box::new(p))).await {
            Ok(_) => t.migrated.reported = Some((role.migration.clone(), phase)),
            Err(e) => tracing::error!(error = %e, "a migration's report could not be journaled"),
        }
    }
}
