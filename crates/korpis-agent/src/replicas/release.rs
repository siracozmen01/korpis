// SPDX-License-Identifier: AGPL-3.0-or-later

//! Releasing what replication no longer needs (§2.2 of docs/06-storage.md). What the node is
//! told is the whole of what it keeps: of a volume of its own, the snapshots its replicas'
//! next increments are based on and those under way; of a replica it keeps, the snapshot it
//! holds and the one being received; of anything else, nothing. Every other snapshot the helper
//! lists is released. A node told nothing yet releases nothing, since it does not know.

use super::Replication;
use crate::host::Replicating;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use std::collections::BTreeSet;

/// The snapshots of `held` that what the node was `told` keeps.
pub(super) fn kept(told: &agent::Replicas, held: &helper::ReplicaHeld) -> BTreeSet<String> {
    if let Some(source) = told.sending.iter().find(|s| s.helper_id == held.volume) {
        let under_way = source.transfers.iter().map(|t| t.snapshot.clone());
        return source.keep.iter().cloned().chain(under_way).collect();
    }
    told.keeping
        .iter()
        .filter(|k| k.volume == held.volume && k.storage_class == held.storage_class)
        .flat_map(|k| [k.holds.clone(), k.receiving.clone()])
        .filter(|s| !s.is_empty())
        .collect()
}

/// Releases every snapshot the helper lists that what the node was told does not keep.
pub(super) async fn release<H: Replicating>(r: &Replication<H>) {
    let Some(told) = r.shared().told.clone() else { return };
    let held = match r.host.replica_volumes().await {
        Ok(held) => held,
        Err(e) => {
            tracing::warn!(error = %e, "what replication keeps here could not be listed");
            return;
        }
    };
    for h in &held {
        let keep = kept(&told, h);
        for snapshot in h.snapshots.iter().filter(|s| !keep.contains(*s)) {
            let request = helper::ReleaseReplicaRequest {
                volume: h.volume.clone(),
                storage_class: h.storage_class.clone(),
                snapshot: snapshot.clone(),
                ..Default::default()
            };
            match r.host.release_replica(&request).await {
                Ok(_) => tracing::info!(volume = %h.volume, %snapshot, "replica snapshot released"),
                Err(e) => tracing::warn!(volume = %h.volume, %snapshot, error = %e, "not released"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(volume: &str, snapshots: &[&str]) -> helper::ReplicaHeld {
        helper::ReplicaHeld {
            storage_class: "local".into(),
            volume: volume.into(),
            snapshots: snapshots.iter().map(|s| (*s).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn what_the_node_is_told_is_all_it_keeps() {
        let told = agent::Replicas {
            sending: vec![agent::ReplicaSource {
                volume: "vol_1".into(),
                helper_id: "wl_1.data".into(),
                keep: vec!["s1".into()],
                transfers: vec![agent::ReplicaTransfer {
                    snapshot: "s2".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            keeping: vec![agent::ReplicaKept {
                volume: "vol_2".into(),
                storage_class: "local".into(),
                holds: "s4".into(),
                receiving: "s5".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let own = kept(&told, &held("wl_1.data", &["s0", "s1", "s2"]));
        assert_eq!(own, BTreeSet::from(["s1".to_owned(), "s2".to_owned()]), "bases and under way");
        let replica = kept(&told, &held("vol_2", &["s3", "s4"]));
        assert_eq!(replica, BTreeSet::from(["s4".to_owned(), "s5".to_owned()]));
        assert!(kept(&told, &held("vol_3", &["s6"])).is_empty(), "a replica no longer kept");
    }
}
