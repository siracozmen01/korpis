// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's migrations as the API shows them (§8.1 of docs/05-scheduling.md): the phase, the
//! hold, how it ended, and each volume with how it crosses, how far it has, and whether the two
//! sides' manifests agree.

use crate::{Result, duration, id, invalid, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::migration::{
    Chunk, Crossing, Migration, MigrationRole, MigrationVolume, Outcome, PassRole, Phase, Progress,
    Side, Standing, VolumeProgress, Watermark,
};
use korpis_proto::korpis::agent::v1 as agent;
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

pub(crate) const fn crossing_to_pb(c: Crossing) -> pb::MigrationCrossing {
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

/// The agent protocol's form of a phase.
#[must_use]
pub const fn agent_phase_to_pb(p: Phase) -> agent::MigrationPhase {
    use agent::MigrationPhase as P;
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

/// The phase the agent protocol's `p` names, if one.
#[must_use]
pub fn agent_phase_from_pb(p: buffa::EnumValue<agent::MigrationPhase>) -> Option<Phase> {
    Phase::ALL.into_iter().find(|phase| p.as_known() == Some(agent_phase_to_pb(*phase)))
}

/// The agent protocol's form of a side.
#[must_use]
pub const fn side_to_pb(s: Side) -> agent::MigrationSide {
    match s {
        Side::Source => agent::MigrationSide::MIGRATION_SIDE_SOURCE,
        Side::Destination => agent::MigrationSide::MIGRATION_SIDE_DESTINATION,
    }
}

/// The side the agent protocol's `s` names, if one.
#[must_use]
pub fn side_from_pb(s: buffa::EnumValue<agent::MigrationSide>) -> Option<Side> {
    [Side::Source, Side::Destination]
        .into_iter()
        .find(|side| s.as_known() == Some(side_to_pb(*side)))
}

fn pass_to_pb(p: &PassRole) -> agent::MigrationPass {
    agent::MigrationPass {
        pass: p.pass.to_string(),
        parent: p.parent.map(|u| u.to_string()).unwrap_or_default(),
        ticket: p.ticket.clone(),
        expires_at: timestamp(p.expires_at).into(),
        ..Default::default()
    }
}

/// A node's part in a migration, as its assignment carries it.
#[must_use]
pub fn role_to_pb(r: &MigrationRole) -> agent::MigrationRole {
    agent::MigrationRole {
        role: side_to_pb(r.side).into(),
        phase: agent_phase_to_pb(r.phase).into(),
        peer: wire_id(IdKind::Node, r.peer),
        plan: wire_id(IdKind::Plan, r.plan),
        migration: wire_id(IdKind::Migration, r.migration),
        volumes: r
            .volumes
            .iter()
            .map(|v| agent::MigrationVolumeRole {
                name: v.name.clone(),
                source_helper_id: v.source_helper_id.clone(),
                crossing: crossing_to_pb(v.crossing).into(),
                volume: wire_id(IdKind::Volume, v.volume),
                pass: v.pass.as_ref().map(pass_to_pb).into(),
                ..Default::default()
            })
            .collect(),
        watermarks: r
            .watermarks
            .iter()
            .map(|w| agent::StreamWatermark {
                stream: w.stream.clone(),
                next_offset: w.next_offset,
                journaled_to: w.journaled_to,
                ..Default::default()
            })
            .collect(),
        hold_until: r.hold_until.map(timestamp).into(),
        ..Default::default()
    }
}

fn volume_progress_from_pb(
    field: &str,
    v: &agent::MigrationVolumeProgress,
) -> Result<VolumeProgress> {
    let pass = match v.pass.as_str() {
        "" => None,
        p => Some(uuid::Uuid::parse_str(p).map_err(|_| invalid(&format!("{field}.pass"), p))?),
    };
    let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    Ok(VolumeProgress {
        name: v.name.clone(),
        pass,
        sent_bytes: (v.sent_bytes > 0).then_some(v.sent_bytes),
        held: v.held,
        helper_id: text(&v.helper_id),
        manifest: text(&v.manifest),
        chunks: v
            .chunks
            .iter()
            .map(|c| Chunk { first_path: c.first_path.clone(), digest: c.digest.clone() })
            .collect(),
    })
}

/// A node's report of its side of a migration's phase, from the field `field`.
///
/// # Errors
///
/// A field naming no migration, phase, or side.
pub fn progress_from_pb(field: &str, p: &agent::MigrationProgress) -> Result<Progress> {
    use agent::MigrationOutcomeOfPhase as O;
    let migration = id(IdKind::Migration, &format!("{field}.migration"), &p.migration)?;
    let phase = agent_phase_from_pb(p.phase)
        .ok_or_else(|| invalid(&format!("{field}.phase"), "no phase"))?;
    let side = side_from_pb(p.side).ok_or_else(|| invalid(&format!("{field}.side"), "no side"))?;
    let standing = match p.outcome.as_known() {
        Some(O::MIGRATION_OUTCOME_OF_PHASE_DONE) => Standing::Done,
        Some(O::MIGRATION_OUTCOME_OF_PHASE_FAILED) => Standing::Failed,
        _ => Standing::Going,
    };
    let volumes = p
        .volumes
        .iter()
        .enumerate()
        .map(|(i, v)| volume_progress_from_pb(&format!("{field}.volumes[{i}]"), v))
        .collect::<Result<_>>()?;
    let watermarks = p
        .watermarks
        .iter()
        .map(|w| Watermark {
            stream: w.stream.clone(),
            next_offset: w.next_offset,
            journaled_to: w.journaled_to,
        })
        .collect();
    Ok(Progress { migration, phase, side, standing, detail: p.detail.clone(), volumes, watermarks })
}
