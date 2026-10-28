// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's report of its side of a migration's phase, as its journal carries it (§8.1 of
//! docs/05-scheduling.md), and where two manifests, each reported in runs of entries, first
//! differ.

use super::{Phase, Side, Watermark};
use uuid::Uuid;

/// How a side's phase stands, as it reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Under way: the report says how far it has come.
    Going,
    /// Done.
    Done,
    /// Failed.
    Failed,
}

/// One run of a manifest's entries: its first path and its digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// The first entry's path.
    pub first_path: String,
    /// The digest of the run.
    pub digest: String,
}

/// One volume's part in a report.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VolumeProgress {
    /// The volume's name in the workload's declaration.
    pub name: String,
    /// The pass, by the identifier its ticket names.
    pub pass: Option<Uuid>,
    /// On the source: what the pass sent.
    pub sent_bytes: Option<u64>,
    /// On the destination: whether the pass is held whole.
    pub held: bool,
    /// On the destination: the helper's identifier for the volume there.
    pub helper_id: Option<String>,
    /// At `verify`: the digest of its manifest.
    pub manifest: Option<String>,
    /// At `verify`: its manifest in runs.
    pub chunks: Vec<Chunk>,
}

/// A node's report of its side of a phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The migration.
    pub migration: Uuid,
    /// The phase it reports.
    pub phase: Phase,
    /// The side it reports as.
    pub side: Side,
    /// How the phase stands.
    pub standing: Standing,
    /// Why it failed.
    pub detail: String,
    /// Each volume's part.
    pub volumes: Vec<VolumeProgress>,
    /// At `quiesce`, from the source: each stream's watermark.
    pub watermarks: Vec<Watermark>,
}

/// The first path of the first run where `source` and `destination` differ, or, where every run
/// they both have agrees, the first path of the first run one has and the other lacks; `None`
/// where they are the same runs.
#[must_use]
pub fn first_difference(source: &[Chunk], destination: &[Chunk]) -> Option<String> {
    let mut d = destination.iter();
    for s in source {
        match d.next() {
            Some(o) if o == s => {}
            Some(o) => return Some(s.first_path.clone().min(o.first_path.clone())),
            None => return Some(s.first_path.clone()),
        }
    }
    d.next().map(|o| o.first_path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(path: &str, digest: &str) -> Chunk {
        Chunk { first_path: path.into(), digest: digest.into() }
    }

    #[test]
    fn two_manifests_first_differ_at_the_first_run_whose_digests_differ() {
        let source = [run("", "a"), run("plugins/", "b"), run("world/region", "c")];
        assert_eq!(first_difference(&source, &source), None);
        let changed = [run("", "a"), run("plugins/", "x"), run("world/region", "c")];
        assert_eq!(first_difference(&source, &changed).as_deref(), Some("plugins/"));
        let short = [run("", "a"), run("plugins/", "b")];
        assert_eq!(first_difference(&source, &short).as_deref(), Some("world/region"));
        assert_eq!(first_difference(&short, &source).as_deref(), Some("world/region"));
    }
}
