// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each node is told of replicas (§2.2 of docs/06-storage.md): a volume's node, each
//! transfer under way from it, with the snapshots it keeps for them; a replica's node, the
//! snapshot it keeps and the one being sent to it, or the one a volume is being made from.
//! Everything else of a volume's replication a
//! node holds it releases. A node told nothing of a volume releases all of it, so what it is told
//! is the whole of what it keeps, and is computed again from the replicas as they are now.

use super::replica::{Replica, TRANSFER_WINDOW};
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

/// A replica, with the volume it is of as its node knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flow {
    /// The replica.
    pub replica: Replica,
    /// The volume's node.
    pub from: Uuid,
    /// The helper's identifier for the volume on its node.
    pub helper_id: String,
    /// The volume's class, which the replica is received into on its own node.
    pub storage_class: String,
}

/// A volume being made from its replica on the node it failed over to, which keeps the
/// replica's snapshot it is made from until it reports the volume made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promoting {
    /// The volume.
    pub volume: Uuid,
    /// Its node now, which keeps the replica.
    pub node: Uuid,
    /// The volume's class, which the replica is kept in.
    pub storage_class: String,
    /// The snapshot it is made from.
    pub snapshot: Uuid,
}

/// What a node is told of one of its volumes that keeps replicas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// The volume.
    pub volume: Uuid,
    /// The helper's identifier for it.
    pub helper_id: String,
    /// The snapshots it keeps: each the base of a replica's next increment, or under way to
    /// one; every other snapshot it sent of the volume it releases.
    pub keep: Vec<Uuid>,
    /// The transfers under way from it.
    pub transfers: Vec<Outbound>,
}

/// One transfer under way from a volume's node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    /// The node receiving it.
    pub to: Uuid,
    /// The snapshot it carries, which the sending node takes when it starts.
    pub snapshot: Uuid,
    /// The snapshot it is an increment from; `None` for a whole one.
    pub parent: Option<Uuid>,
    /// When it is given up.
    pub expires_at: SystemTime,
    /// The ticket the receiving node admits it by, signed by the control plane; filled in by
    /// the component holding the key.
    pub ticket: String,
}

/// What a node is told of a replica it keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    /// The volume.
    pub volume: Uuid,
    /// The volume's node, which sends to it.
    pub from: Uuid,
    /// The class it is kept in, the volume's.
    pub storage_class: String,
    /// The snapshot it holds and keeps; `None` until the first arrives, or once it is stale.
    pub holds: Option<Uuid>,
    /// The snapshot being sent to it.
    pub receiving: Option<Uuid>,
}

/// What one node is told.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeReplicas {
    /// Its volumes that keep replicas.
    pub sending: Vec<Outgoing>,
    /// The replicas it keeps.
    pub keeping: Vec<Kept>,
}

/// What each node is told, from every replica's `flows` and the volumes `promoting`, by node; a
/// node with nothing to keep is absent. A stale replica is sent whole, so its snapshot is no
/// base: the volume's node no longer keeps it, and the replica keeps it only until the whole
/// copy arrives. A volume being made from its replica is no longer sent from it, and its node
/// keeps the snapshot it is made from as a replica it holds.
#[must_use]
pub fn told(flows: &[Flow], promoting: &[Promoting]) -> BTreeMap<Uuid, NodeReplicas> {
    let mut out: BTreeMap<Uuid, NodeReplicas> = BTreeMap::new();
    for p in promoting {
        out.entry(p.node).or_default().keeping.push(Kept {
            volume: p.volume,
            from: p.node,
            storage_class: p.storage_class.clone(),
            holds: Some(p.snapshot),
            receiving: None,
        });
    }
    let mut sources: BTreeMap<(Uuid, Uuid), Outgoing> = BTreeMap::new();
    for f in flows {
        let r = &f.replica;
        let base = r.held.map(|(s, _)| s).filter(|_| r.stale_at.is_none());
        let o = sources.entry((f.from, r.volume)).or_insert_with(|| Outgoing {
            volume: r.volume,
            helper_id: f.helper_id.clone(),
            keep: Vec::new(),
            transfers: Vec::new(),
        });
        o.keep.extend(base.into_iter().chain(r.sending.map(|(s, _)| s)));
        if let Some((snapshot, since)) = r.sending {
            let expires_at = since + TRANSFER_WINDOW;
            let ticket = String::new();
            o.transfers.push(Outbound { to: r.node, snapshot, parent: base, expires_at, ticket });
        }
        let (holds, receiving) = (r.held.map(|(s, _)| s), r.sending.map(|(s, _)| s));
        out.entry(r.node).or_default().keeping.push(Kept {
            volume: r.volume,
            from: f.from,
            storage_class: f.storage_class.clone(),
            holds,
            receiving,
        });
    }
    for ((from, _), mut o) in sources {
        o.keep.sort_unstable();
        o.keep.dedup();
        out.entry(from).or_default().sending.push(o);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_volume_s_node_keeps_each_base_and_sends_a_stale_replica_whole() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let (volume, from, b, c) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());
        let replica = |node, held, sending, stale_at| Replica {
            volume,
            node,
            chosen_at: t0,
            held,
            sending,
            stale_at,
        };
        let flows = [
            Flow {
                replica: replica(b, Some((s1, t0)), Some((s2, t0)), None),
                from,
                helper_id: "v".into(),
                storage_class: "local".into(),
            },
            Flow {
                replica: replica(c, Some((s1, t0)), Some((s2, t0)), Some(t0)),
                from,
                helper_id: "v".into(),
                storage_class: "local".into(),
            },
        ];
        let (other, s0) = (Uuid::now_v7(), Uuid::now_v7());
        let made =
            Promoting { volume: other, node: b, storage_class: "local".into(), snapshot: s0 };
        let told = told(&flows, &[made]);
        let sent = &told[&from].sending;
        assert_eq!(sent.len(), 1, "one volume");
        let mut keep = vec![s1, s2];
        keep.sort_unstable();
        assert_eq!(sent[0].keep, keep, "the base of b's increment and what is under way");
        let parents: Vec<_> = sent[0].transfers.iter().map(|t| (t.to, t.parent)).collect();
        assert_eq!(parents, [(b, Some(s1)), (c, None)], "the stale one whole");
        assert_eq!(told[&c].keeping[0].receiving, Some(s2));
        assert!(told[&from].keeping.is_empty() && told[&b].sending.is_empty());
        let held: Vec<_> = told[&b].keeping.iter().map(|k| (k.volume, k.holds)).collect();
        assert!(held.contains(&(other, Some(s0))), "the snapshot a volume is made from");
    }
}
