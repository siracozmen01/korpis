// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's migration (§8 and §8.1 of docs/05-scheduling.md): its seven phases in order, how
//! it ended, and how each of its volumes crosses from the source to the destination, decided by
//! the classes both nodes declared. A phase is a desired state the control plane sets on both
//! nodes' assignments and advances only on their reports; nothing here talks to a node.

use crate::node::driver::{StorageClass, StorageFeature};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How long the source keeps its copies after cutover when the migration says nothing: a day,
/// long enough for a workload that fails in production to fail.
pub const DEFAULT_HOLD: Duration = Duration::from_secs(24 * 3600);

/// The longest hold a migration may ask for: a month, past which a copy kept for a rollback is a
/// copy nobody remembers keeping.
pub const LONGEST_HOLD: Duration = Duration::from_secs(31 * 24 * 3600);

/// The least a pass may send and still be large, in bytes: below this, a pass is small whatever
/// the volume's size.
pub const SMALL_PASS: u64 = 64 << 20;

/// The pass after which `replicate` ends whatever the last one sent.
pub const MOST_PASSES: u32 = 10;

/// A migration's phase, in the order it moves through them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// The destination accepts the target and makes room for each volume.
    Prepare,
    /// The volumes are copied, pass after pass, while the workload runs.
    Replicate,
    /// The workload stops on the source, or its state is captured.
    Quiesce,
    /// The last pass, taken stopped.
    FinalSync,
    /// Both nodes' manifests are compared, and the destination prepares with its volumes.
    Verify,
    /// The source releases its lease and the destination is granted the next.
    Cutover,
    /// The source keeps its copies until the hold ends, then deletes them.
    Release,
}

impl Phase {
    /// Every phase, in order.
    pub const ALL: [Self; 7] = [
        Self::Prepare,
        Self::Replicate,
        Self::Quiesce,
        Self::FinalSync,
        Self::Verify,
        Self::Cutover,
        Self::Release,
    ];

    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Replicate => "replicate",
            Self::Quiesce => "quiesce",
            Self::FinalSync => "final_sync",
            Self::Verify => "verify",
            Self::Cutover => "cutover",
            Self::Release => "release",
        }
    }

    /// The phase named `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// The phase after it; `None` after `release`.
    #[must_use]
    pub fn next(self) -> Option<Self> {
        Self::ALL.into_iter().skip_while(|p| *p != self).nth(1)
    }

    /// Whether the placement has moved to the destination in this phase: from `cutover` on.
    #[must_use]
    pub fn moved(self) -> bool {
        self >= Self::Cutover
    }
}

/// How a migration ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The workload runs on the destination, and the source's copies are gone.
    Completed,
    /// The workload was moved back to the source after cutover.
    RolledBack,
    /// It ended before cutover, the source untouched.
    Abandoned,
}

impl Outcome {
    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::RolledBack => "rolled_back",
            Self::Abandoned => "abandoned",
        }
    }

    /// The outcome named `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Completed, Self::RolledBack, Self::Abandoned].into_iter().find(|o| o.as_str() == s)
    }
}

/// How a volume crosses from the source to the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crossing {
    /// By increments between snapshots, as a replica is sent (§2.2 of docs/06-storage.md).
    Increments,
    /// By rsync over the tree, pass after pass.
    Tree,
    /// Not at all: the destination reaches the same storage (§2.1 of docs/06-storage.md).
    Nothing,
}

impl Crossing {
    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Increments => "increments",
            Self::Tree => "tree",
            Self::Nothing => "nothing",
        }
    }

    /// The crossing named `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Increments, Self::Tree, Self::Nothing].into_iter().find(|c| c.as_str() == s)
    }

    /// Whether anything is sent for it, and so whether its copies are compared.
    #[must_use]
    pub const fn sends(self) -> bool {
        !matches!(self, Self::Nothing)
    }
}

/// Why a volume cannot cross.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Uncrossable {
    /// The destination offers no class of the volume's name.
    NotOffered,
    /// Neither both nodes send increments of the same backend, nor both sync trees; names the
    /// nodes lacking `tree_sync`, `source` or `destination`.
    NoTreeSync(Vec<&'static str>),
}

impl std::fmt::Display for Uncrossable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOffered => f.write_str("the destination does not offer its class"),
            Self::NoTreeSync(lacking) => write!(
                f,
                "its class sends no increments between the two nodes, and the {} cannot sync \
                 its tree (rsync, §2 of docs/18-operations.md)",
                lacking.join(" and the ")
            ),
        }
    }
}

/// How a volume in `source`, the class it is in on the source, crosses to a destination whose
/// class of that name is `destination`.
///
/// # Errors
///
/// [`Uncrossable`] when it cannot.
pub fn crossing(
    source: &StorageClass,
    destination: Option<&StorageClass>,
) -> Result<Crossing, Uncrossable> {
    let to = destination.ok_or(Uncrossable::NotOffered)?;
    if source.block.is_some() || source.store.is_some() {
        return Ok(Crossing::Nothing);
    }
    let increments = |c: &StorageClass| c.capabilities.has(StorageFeature::IncrementalSend);
    if increments(source) && increments(to) && source.backend == to.backend {
        return Ok(Crossing::Increments);
    }
    let tree = |c: &StorageClass| c.capabilities.has(StorageFeature::TreeSync);
    let lacking: Vec<&'static str> = [(source, "source"), (to, "destination")]
        .into_iter()
        .filter(|(c, _)| !tree(c))
        .map(|(_, side)| side)
        .collect();
    if lacking.is_empty() { Ok(Crossing::Tree) } else { Err(Uncrossable::NoTreeSync(lacking)) }
}

/// Whether a pass that sent `sent` bytes of a volume of `size` bytes, the `pass`th, ends
/// `replicate`: it sent at most a hundredth of the volume or [`SMALL_PASS`], whichever is
/// larger, or it is the [`MOST_PASSES`]th.
#[must_use]
pub fn small(sent: u64, size: u64, pass: u32) -> bool {
    sent <= (size / 100).max(SMALL_PASS) || pass >= MOST_PASSES
}

/// A migration, as the control plane keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// Its identifier.
    pub id: Uuid,
    /// The plan of kind `migrate` it carries out.
    pub plan: Uuid,
    /// The workload.
    pub workload: Uuid,
    /// The node it leaves.
    pub source: Uuid,
    /// The node it goes to.
    pub destination: Uuid,
    /// Its phase.
    pub phase: Phase,
    /// When it entered the phase.
    pub phase_since: SystemTime,
    /// How long the source keeps its copies after cutover.
    pub hold: Duration,
    /// When the hold ends; set as it enters `release`.
    pub hold_until: Option<SystemTime>,
    /// How it ended, and why; `None` while it runs.
    pub outcome: Option<(Outcome, String)>,
    /// When it was made.
    pub made_at: SystemTime,
    /// The last phase each side reported done.
    pub reported: Reported,
    /// When the placement moved to the destination at `cutover`; `None` before, and again once
    /// a rollback moved it back.
    pub moved_at: Option<SystemTime>,
}

/// One volume of a migration: how it crosses, and how far it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationVolume {
    /// The volume.
    pub volume: Uuid,
    /// The name the workload's declaration gives it.
    pub name: String,
    /// How it crosses.
    pub crossing: Crossing,
    /// Its size when the migration was made.
    pub size_bytes: u64,
    /// The helper's identifier for it on the source.
    pub source_helper_id: String,
    /// The passes sent so far.
    pub passes: u32,
    /// The pass under way, and since when.
    pub pass: Option<(Uuid, SystemTime)>,
    /// The pass the one under way is an increment from, which the destination holds.
    pub parent_pass: Option<Uuid>,
    /// What the pass under way sent, once the source reported it.
    pub sent_bytes: Option<u64>,
    /// The last pass the destination reported held.
    pub held_pass: Option<Uuid>,
    /// The helper's identifier for it on the destination, once it reported one.
    pub destination_helper_id: Option<String>,
    /// Each side's manifest digest, source then destination, once reported.
    pub manifests: (Option<String>, Option<String>),
    /// The first entry the two manifests differ in, when they did.
    pub first_difference: Option<String>,
}

impl MigrationVolume {
    /// A volume about to cross, nothing sent yet.
    #[must_use]
    pub fn new(volume: Uuid, name: String, crossing: Crossing, size: (u64, String)) -> Self {
        let (size_bytes, source_helper_id) = size;
        Self {
            volume,
            name,
            crossing,
            size_bytes,
            source_helper_id,
            passes: 0,
            pass: None,
            parent_pass: None,
            sent_bytes: None,
            held_pass: None,
            destination_helper_id: None,
            manifests: (None, None),
            first_difference: None,
        }
    }

    /// Whether both sides' manifests arrived and agree; true of a volume nothing is sent for.
    #[must_use]
    pub fn verified(&self) -> bool {
        !self.crossing.sends() || matches!(&self.manifests, (Some(s), Some(d)) if s == d)
    }
}

impl Migration {
    /// Whether it runs: it has not ended.
    #[must_use]
    pub const fn live(&self) -> bool {
        self.outcome.is_none()
    }

    /// The node the workload is placed on, authoritatively: the destination once the placement
    /// moved at `cutover`, the source before.
    #[must_use]
    pub const fn authoritative(&self) -> Uuid {
        if self.moved_at.is_some() { self.destination } else { self.source }
    }
}

mod passes;
mod progress;
mod report;
mod role;

pub use passes::NewPass;
pub use progress::{Next, Reported, Side, acts, failed, next};
pub use report::{Chunk, Progress, Standing, VolumeProgress, first_difference};
pub use role::{MigrationRole, PASS_LIFETIME, PassRole, VolumeRole, Watermark};

#[cfg(test)]
mod tests;
