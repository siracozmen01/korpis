// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's part in a migration, as its assignment of the workload carries it (§4.8 of
//! docs/02-architecture.md, §8.1 of docs/05-scheduling.md): which side it is, of which
//! migration, in which phase, with which peer, what crosses for each volume, the streams'
//! watermarks once the destination needs them, and when the source's hold ends.

use super::{Crossing, Migration, MigrationVolume, Phase, Side};
use std::time::SystemTime;
use uuid::Uuid;

/// One stream's watermark at `quiesce` (§4 of docs/14-streams.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    /// The stream, by the name its node watches it under.
    pub stream: String,
    /// The offset its next record takes.
    pub next_offset: u64,
    /// The position in its changes log the source's agent journaled and offloaded to.
    pub journaled_to: u64,
}

/// How one volume crosses, as a node is told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRole {
    /// The volume's name in the workload's declaration.
    pub name: String,
    /// The helper's identifier for it on the source.
    pub source_helper_id: String,
    /// How it crosses.
    pub crossing: Crossing,
}

/// A node's part in a migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRole {
    /// The migration.
    pub migration: Uuid,
    /// The plan it carries out.
    pub plan: Uuid,
    /// Which side the node is.
    pub side: Side,
    /// The phase both nodes are in.
    pub phase: Phase,
    /// The other node.
    pub peer: Uuid,
    /// Each volume the target attaches.
    pub volumes: Vec<VolumeRole>,
    /// From `cutover`, on the destination: each stream's watermark.
    pub watermarks: Vec<Watermark>,
    /// When the source deletes its copies; set as the migration enters `release`.
    pub hold_until: Option<SystemTime>,
}

impl MigrationRole {
    /// `side`'s part in `m`, whose volumes are `volumes` and whose streams' watermarks, as the
    /// source reported them, are `watermarks`; the destination is told them from `cutover`.
    #[must_use]
    pub fn of(
        m: &Migration,
        side: Side,
        (volumes, watermarks): (&[MigrationVolume], &[Watermark]),
    ) -> Self {
        let peer = match side {
            Side::Source => m.destination,
            Side::Destination => m.source,
        };
        let handed = side == Side::Destination && m.moved_at.is_some();
        Self {
            migration: m.id,
            plan: m.plan,
            side,
            phase: m.phase,
            peer,
            volumes: volumes
                .iter()
                .map(|v| VolumeRole {
                    name: v.name.clone(),
                    source_helper_id: v.source_helper_id.clone(),
                    crossing: v.crossing,
                })
                .collect(),
            watermarks: if handed { watermarks.to_vec() } else { Vec::new() },
            hold_until: m.hold_until,
        }
    }
}
