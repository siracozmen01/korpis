// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's replicas (§2.2 of docs/06-storage.md): the nodes the control plane chose to keep a
//! copy, what each last received, and the choice of them. A replica is chosen as a placement is,
//! by what the node offers and has room for, and kept from one interval to the next: moving one
//! means sending the volume whole again.

use crate::node::driver::{StorageFeature, class_named};
use crate::node::{Connectivity, Node};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How many intervals a replica may go without receiving before it is stale: its snapshot on the
/// volume's node is released, and it is sent whole when it returns.
pub const STALE_AFTER: u32 = 10;

/// How long one transfer may take: the life of its ticket, after which a transfer not received
/// is given up and sent again. Long enough for a large first copy over a slow link, and short
/// enough that a ticket taken from a node removed stops working within the hour.
pub const TRANSFER_WINDOW: Duration = Duration::from_secs(3600);

/// One replica of a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replica {
    /// The volume.
    pub volume: Uuid,
    /// The node keeping it.
    pub node: Uuid,
    /// When the control plane chose the node.
    pub chosen_at: SystemTime,
    /// The snapshot it holds whole, and when it was received; `None` until the first.
    pub held: Option<(Uuid, SystemTime)>,
    /// The snapshot being sent to it, and since when.
    pub sending: Option<(Uuid, SystemTime)>,
    /// When it was marked stale.
    pub stale_at: Option<SystemTime>,
}

impl Replica {
    /// Whether, at `now`, it has gone [`STALE_AFTER`] intervals of `interval` without receiving
    /// since it was chosen or last received, and is not yet marked so.
    #[must_use]
    pub fn goes_stale(&self, interval: Duration, now: SystemTime) -> bool {
        let since = self.held.map_or(self.chosen_at, |(_, at)| at);
        self.stale_at.is_none() && since + interval.saturating_mul(STALE_AFTER) <= now
    }

    /// Whether, at `now`, a transfer to it is due: none is under way, or the one under way
    /// outlived its ticket; and it holds nothing yet, or what it holds is an `interval` old.
    #[must_use]
    pub fn due(&self, interval: Duration, now: SystemTime) -> bool {
        let idle = self.sending.is_none_or(|(_, since)| since + TRANSFER_WINDOW <= now);
        idle && self.held.is_none_or(|(_, at)| at + interval <= now)
    }

    /// How old what it holds is at `now`: what the volume lost since, were it started there.
    #[must_use]
    pub fn age(&self, now: SystemTime) -> Option<Duration> {
        self.held.map(|(_, at)| now.duration_since(at).unwrap_or_default())
    }
}

/// What a replica of a volume needs of its node.
#[derive(Debug, Clone, Copy)]
pub struct Wanted<'a> {
    /// The node holding the volume, which keeps no replica of it.
    pub on: &'a Node,
    /// The volume's class, by name; empty for each node's default.
    pub class: &'a str,
    /// Its size, which the node's class must have free.
    pub size: u64,
    /// The labels whose values are failure domains, most significant first: those the owners'
    /// spread constraints name, then `region`.
    pub domains: &'a [String],
}

/// Whether `n` can keep a replica `w` describes: another node, its class of that name able to
/// send increments with room for the volume.
#[must_use]
pub fn can_keep(w: &Wanted<'_>, n: &Node) -> bool {
    still_keeps(w, n)
        && class_named(&n.storage_classes, w.class).is_some_and(|c| c.free_bytes >= w.size)
}

/// Whether `n`, keeping a replica `w` describes, may go on keeping it: another node, its class
/// of that name able to send increments. Its room is not asked again, since the replica's own
/// bytes take it.
#[must_use]
pub fn still_keeps(w: &Wanted<'_>, n: &Node) -> bool {
    n.id != w.on.id
        && class_named(&n.storage_classes, w.class)
            .is_some_and(|c| c.capabilities.has(StorageFeature::IncrementalSend))
}

/// What one pass changes of a volume's replicas, by node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tending {
    /// Replicas no longer kept: their node no longer serves the volume's organization or can
    /// keep it, the volume asks for fewer, or none.
    pub dropped: Vec<Uuid>,
    /// Replicas marked stale now.
    pub stale: Vec<Uuid>,
    /// Nodes chosen to keep new ones.
    pub chosen: Vec<Uuid>,
    /// Replicas staying, reached now, that a transfer is due to.
    pub due: Vec<Uuid>,
}

/// What a pass changes of the replicas `kept` of the volume `w` describes, at `now`: `want`
/// says how many it asks for and at what interval, `None` for none; `serving` are the nodes
/// serving its organization, and `candidates` those of them ready, with room on their disk. A
/// replica stale is still one of them: it holds the volume as it was, and is sent whole when it
/// returns. Of more than are asked for, those holding the newest copy stay.
#[must_use]
pub fn tend(
    w: &Wanted<'_>,
    want: Option<(usize, Duration)>,
    kept: &[Replica],
    (serving, candidates): (&[&Node], &[&Node]),
    now: SystemTime,
) -> Tending {
    let Some((count, interval)) = want else {
        return Tending { dropped: kept.iter().map(|r| r.node).collect(), ..Tending::default() };
    };
    let node = |id: Uuid| serving.iter().copied().find(|n| n.id == id);
    let (mut staying, gone): (Vec<&Replica>, Vec<&Replica>) =
        kept.iter().partition(|r| node(r.node).is_some_and(|n| still_keeps(w, n)));
    let mut dropped: Vec<Uuid> = gone.iter().map(|r| r.node).collect();
    staying.sort_by_key(|r| {
        (r.stale_at.is_some(), std::cmp::Reverse(r.held.map(|(_, at)| at)), r.chosen_at, r.node)
    });
    let extra = staying.split_off(count.min(staying.len()));
    dropped.extend(extra.iter().map(|r| r.node));
    let stale = staying.iter().filter(|r| r.goes_stale(interval, now)).map(|r| r.node).collect();
    let on: Vec<&Node> = staying.iter().filter_map(|r| node(r.node)).collect();
    let chosen = choose(w, candidates, &on, count.saturating_sub(staying.len()));
    let reached = |n: &Node| n.connectivity == Connectivity::Connected;
    let due = if reached(w.on) {
        let ready = staying.iter().filter(|r| node(r.node).is_some_and(reached));
        ready.filter(|r| r.due(interval, now)).map(|r| r.node).collect()
    } else {
        Vec::new()
    };
    Tending { dropped, stale, chosen, due }
}

/// What a node reports of a replica it keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedReplica {
    /// The volume.
    pub volume: Uuid,
    /// The snapshot sent to it.
    pub snapshot: Uuid,
    /// How the receive ended.
    pub event: ReplicaEvent,
}

/// How a receive ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicaEvent {
    /// The snapshot is held whole, read-only.
    Received,
    /// It ended short, and nothing of it is kept: why.
    Failed(String),
}

/// Up to `count` nodes of `candidates` to keep new replicas of the volume `w` describes, besides
/// those in `kept`: each one able to, and none already keeping one; those sharing the fewest
/// failure domains with the volume's node and the replicas kept go first, then those with the
/// most room, then the lower identifier, so the same inputs always choose the same nodes.
#[must_use]
pub fn choose(w: &Wanted<'_>, candidates: &[&Node], kept: &[&Node], count: usize) -> Vec<Uuid> {
    let mut chosen: Vec<&Node> = Vec::new();
    while chosen.len() < count {
        let taken = |n: &Node| kept.iter().chain(&chosen).any(|k| k.id == n.id);
        let next = candidates
            .iter()
            .copied()
            .filter(|n| can_keep(w, n) && !taken(n))
            .min_by_key(|n| (shared(w, n, kept, &chosen), std::cmp::Reverse(room(w, n)), n.id));
        match next {
            Some(n) => chosen.push(n),
            None => break,
        }
    }
    chosen.iter().map(|n| n.id).collect()
}

/// How many failure domains `n` shares with the volume's node and the replicas already kept or
/// chosen, the most significant key weighing most.
fn shared(w: &Wanted<'_>, n: &Node, kept: &[&Node], chosen: &[&Node]) -> u64 {
    let mut score = 0u64;
    for (rank, key) in w.domains.iter().enumerate() {
        let weight = 1u64 << (w.domains.len().saturating_sub(rank).min(32));
        let mine = n.labels.as_map().get(key);
        let others =
            std::iter::once(w.on).chain(kept.iter().copied()).chain(chosen.iter().copied());
        let same = others.filter(|o| mine.is_some() && o.labels.as_map().get(key) == mine).count();
        score = score.saturating_add(weight.saturating_mul(u64::try_from(same).unwrap_or(0)));
    }
    score
}

/// The free bytes of `n`'s class for the volume.
fn room(w: &Wanted<'_>, n: &Node) -> u64 {
    class_named(&n.storage_classes, w.class).map_or(0, |c| c.free_bytes)
}
