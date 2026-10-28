// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's migrations as the API shows them (§8.1 of docs/05-scheduling.md): the phase, the
//! hold, how it ended, and each volume with how it crosses, how far it has, and whether the two
//! sides' manifests agree.

use crate::{duration, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::migration::{Crossing, Migration, MigrationVolume, Outcome, Phase};
use korpis_proto::korpis::v1 as pb;

/// The wire form of a phase.
#[must_use]
pub const fn phase_to_pb(p: Phase) -> pb::MigrationPhase {
    use pb::MigrationPhase as P;
    match p {
        Phase::Prepare => P::MIGRATION_PHASE_PREPARE,
        Phase::Replicate => P::MIGRATION_PHASE_REPLICATE,
        Phase::Quiesce => P::MIGRATION_PHASE_QUIESCE,
        Phase::FinalSync => P::MIGRATION_PHASE_FINAL_SYNC,
        Phase::Verify => P::MIGRATION_PHASE_VERIFY,
        Phase::Cutover => P::MIGRATION_PHASE_CUTOVER,
        Phase::Release => P::MIGRATION_PHASE_RELEASE,
    }
}

const fn outcome_to_pb(o: Option<Outcome>) -> pb::MigrationOutcome {
    use pb::MigrationOutcome as O;
    match o {
        None => O::MIGRATION_OUTCOME_UNSPECIFIED,
        Some(Outcome::Completed) => O::MIGRATION_OUTCOME_COMPLETED,
        Some(Outcome::RolledBack) => O::MIGRATION_OUTCOME_ROLLED_BACK,
        Some(Outcome::Abandoned) => O::MIGRATION_OUTCOME_ABANDONED,
    }
}

const fn crossing_to_pb(c: Crossing) -> pb::MigrationCrossing {
    use pb::MigrationCrossing as C;
    match c {
        Crossing::Increments => C::MIGRATION_CROSSING_INCREMENTS,
        Crossing::Tree => C::MIGRATION_CROSSING_TREE,
        Crossing::Nothing => C::MIGRATION_CROSSING_NOTHING,
    }
}

fn volume_to_pb(v: &MigrationVolume) -> pb::MigrationVolume {
    pb::MigrationVolume {
        volume: wire_id(IdKind::Volume, v.volume),
        name: v.name.clone(),
        crossing: crossing_to_pb(v.crossing).into(),
        size_bytes: v.size_bytes,
        passes: v.passes,
        sent_bytes: v.sent_bytes.unwrap_or(0),
        verified: v.crossing.sends() && v.verified(),
        first_difference: v.first_difference.clone().unwrap_or_default(),
        ..Default::default()
    }
}

/// The wire form of a migration and its volumes.
#[must_use]
pub fn migration_to_pb(m: &Migration, volumes: &[MigrationVolume]) -> pb::Migration {
    let (outcome, reason) = match &m.outcome {
        Some((o, r)) => (Some(*o), r.clone()),
        None => (None, String::new()),
    };
    pb::Migration {
        id: wire_id(IdKind::Migration, m.id),
        plan: wire_id(IdKind::Plan, m.plan),
        workload: wire_id(IdKind::Workload, m.workload),
        source: wire_id(IdKind::Node, m.source),
        destination: wire_id(IdKind::Node, m.destination),
        phase: phase_to_pb(m.phase).into(),
        phase_since: timestamp(m.phase_since).into(),
        hold: duration(m.hold).into(),
        hold_until: m.hold_until.map(timestamp).into(),
        outcome: outcome_to_pb(outcome).into(),
        reason,
        made_at: timestamp(m.made_at).into(),
        volumes: volumes.iter().map(volume_to_pb).collect(),
        ..Default::default()
    }
}
