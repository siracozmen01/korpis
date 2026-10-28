// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each side of a migration reports, and what the control plane does next (§8.1 of
//! docs/05-scheduling.md). A phase is done once every side acting in it has reported it done;
//! `verify` is done only where every volume sent arrived as it left; a side reporting a phase
//! failed abandons the migration before `cutover` and rolls it back from then on, since only
//! then does the destination hold the workload.

use super::{Migration, MigrationVolume, Phase};
use uuid::Uuid;

/// A side of a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The node the workload leaves.
    Source,
    /// The node it goes to.
    Destination,
}

impl Side {
    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Destination => "destination",
        }
    }

    /// The side `node` is of `m`, if either.
    #[must_use]
    pub fn of(m: &Migration, node: Uuid) -> Option<Self> {
        if node == m.source {
            Some(Self::Source)
        } else if node == m.destination {
            Some(Self::Destination)
        } else {
            None
        }
    }
}

/// The last phase each side reported done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reported {
    /// The source's.
    pub source: Option<Phase>,
    /// The destination's.
    pub destination: Option<Phase>,
}

impl Reported {
    /// The last phase `side` reported done.
    #[must_use]
    pub const fn by(&self, side: Side) -> Option<Phase> {
        match side {
            Side::Source => self.source,
            Side::Destination => self.destination,
        }
    }
}

/// Whether `side` reports `phase` done, which the phase waits for: the destination prepares,
/// and starts the workload at cutover; the source stops it and, after the hold, deletes its
/// copies; both compute their manifests. Neither reports a copy done: the control plane judges
/// each pass from what both report of it (`MigrationVolume::passed`).
#[must_use]
pub const fn acts(phase: Phase, side: Side) -> bool {
    match phase {
        Phase::Prepare | Phase::Cutover => matches!(side, Side::Destination),
        Phase::Quiesce | Phase::Release => matches!(side, Side::Source),
        Phase::Verify => true,
        Phase::Replicate | Phase::FinalSync => false,
    }
}

/// What the control plane does next with a migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Nothing yet: a side acting in the phase has not reported it done, or a volume's pass is
    /// not judged.
    Wait,
    /// Moves both nodes on to this phase.
    Advance(Phase),
    /// Ends it completed: the source deleted its copies.
    Complete,
    /// Ends it before cutover, for this reason; the source runs on.
    Abandon(String),
    /// Moves the workload back to the source, for this reason.
    RollBack(String),
}

/// The first volume of `volumes` sent whose two manifests both arrived and differ, with the entry
/// they first differ in where it is known.
fn mismatch(volumes: &[MigrationVolume]) -> Option<String> {
    volumes.iter().filter(|v| v.crossing.sends()).find_map(|v| match &v.manifests {
        (Some(s), Some(d)) if s != d => Some(match &v.first_difference {
            Some(at) => format!("volume {} arrived differing at {at}", v.name),
            None => format!("volume {} arrived differing", v.name),
        }),
        _ => None,
    })
}

/// What follows for `m`, whose sides reported `reported`, its volumes as `volumes` say.
#[must_use]
pub fn next(m: &Migration, reported: Reported, volumes: &[MigrationVolume]) -> Next {
    let phase = m.phase;
    if phase == Phase::Verify
        && let Some(why) = mismatch(volumes)
    {
        return Next::Abandon(why);
    }
    let sides = [Side::Source, Side::Destination];
    let done = sides
        .into_iter()
        .filter(|s| acts(phase, *s))
        .all(|s| reported.by(s).is_some_and(|p| p >= phase));
    let verified = phase != Phase::Verify || volumes.iter().all(MigrationVolume::verified);
    let passed = volumes.iter().all(|v| v.passed(phase));
    if !done || !verified || !passed {
        return Next::Wait;
    }
    phase.next().map_or(Next::Complete, Next::Advance)
}

/// What follows a side's report that `phase` failed, for `why`: nothing where the migration is
/// in another phase now, since the report is late, or in `release`, whose deletion the source
/// tries again; otherwise the migration ends, abandoned before cutover and rolled back at it.
#[must_use]
pub fn failed(m: &Migration, phase: Phase, why: &str) -> Next {
    if phase != m.phase || phase == Phase::Release {
        Next::Wait
    } else if phase.moved() {
        Next::RollBack(why.to_owned())
    } else {
        Next::Abandon(why.to_owned())
    }
}
