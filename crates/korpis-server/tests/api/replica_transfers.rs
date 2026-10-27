// SPDX-License-Identifier: AGPL-3.0-or-later

//! Transfers to a volume's replicas (§2.2 of docs/06-storage.md): once its replicas are chosen,
//! the volume's node is told to send each one snapshot, whole, with a ticket the token keys open
//! naming the volume, the two nodes, and the snapshot; each replica's node is told what it
//! receives. A receive reported by the node it was sent to is recorded, and shown as the
//! replica's age; the volume's node then keeps that snapshot as the base of the next increment.
//! A receive reported of a snapshot not under way to the node is refused, and one that failed
//! ends its transfer, so the next pass sends another.

use crate::agent::{Stream, any};
use crate::journal::{ack, send, until};
use crate::replicated::{Replicated, replicas_once};
use korpis_core::authz::token::{KeyRing, parse_key_name};
use korpis_core::authz::transfer::open;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use std::time::SystemTime;

/// The next `Replicas` a node is told that satisfies `pick`.
fn replicas(pick: impl Fn(&agent::Replicas) -> bool) -> impl Fn(&Down) -> Option<agent::Replicas> {
    move |d| match d {
        Down::Replicas(r) if pick(r) => Some((**r).clone()),
        _ => None,
    }
}

/// The next message on `stream` that `pick` takes, reading every message: what a node is told
/// of replicas is passed over by `next`, which `until` reads through.
async fn told_of<T>(stream: &mut Stream, pick: impl Fn(&Down) -> Option<T>) -> T {
    for _ in 0..60 {
        if let Some(found) = any(stream).await.message.as_ref().and_then(&pick) {
            return found;
        }
    }
    panic!("not told within sixty messages");
}

/// A journal record of a receive of `snapshot` into the replica of `volume`.
fn received(seq: u64, volume: &str, snapshot: &str, failed: bool) -> agent::JournalRecord {
    let event = if failed {
        agent::ReplicaEventKind::REPLICA_EVENT_KIND_FAILED
    } else {
        agent::ReplicaEventKind::REPLICA_EVENT_KIND_RECEIVED
    };
    let e = agent::ReplicaEvent {
        volume: volume.to_owned(),
        snapshot: snapshot.to_owned(),
        event: event.into(),
        error: if failed { "the stream ended short".into() } else { String::new() },
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::Replica(Box::new(e))),
        count: 1,
        ..Default::default()
    }
}

/// Asserts that each transfer of `source` is whole, under a ticket the token keys of `r`'s volume
/// node open, naming the volume, the two nodes, and the snapshot.
fn tickets_open(r: &Replicated, source: &agent::ReplicaSource) {
    let mut ring = KeyRing::default();
    for k in &r.a.0.token_keys {
        ring.insert(parse_key_name(&k.key_id).unwrap(), &k.public_key).unwrap();
    }
    let uuid = |kind, id: &str| korpis_wire::id(kind, "id", id).unwrap();
    for t in &source.transfers {
        assert!(t.parent.is_empty(), "whole, the first time");
        let ticket = open(&ring, &t.ticket, SystemTime::now()).expect("the token keys open it");
        assert_eq!(ticket.volume, uuid(IdKind::Volume, &r.volume));
        assert_eq!(
            (ticket.from, ticket.to),
            (uuid(IdKind::Node, &r.a.0.node), uuid(IdKind::Node, &t.to))
        );
        assert_eq!((ticket.snapshot.to_string(), ticket.parent), (t.snapshot.clone(), None));
    }
}

#[tokio::test]
async fn each_replica_is_sent_a_snapshot_under_a_ticket_and_its_receive_recorded() {
    let mut r = Box::pin(Replicated::new()).await;
    replicas_once(&r.h, &r.project, |r| r.len() == 2).await;
    let (b, c) = (r.others[0].0.node.clone(), r.others[1].0.node.clone());
    let sa = &mut r.a.1;
    let told = told_of(sa, replicas(|r| r.sending.first().is_some_and(|s| s.transfers.len() == 2)));
    let source = told.await.sending[0].clone();
    let to = |node: &str| source.transfers.iter().find(|t| t.to == node).unwrap().snapshot.clone();
    // Each sent the snapshot of the pass it became due in: one each, as the nodes came.
    let (snapshot, for_c) = (to(&b), to(&c));
    let mut under_way = vec![snapshot.clone(), for_c.clone()];
    under_way.sort_unstable();
    under_way.dedup();
    assert_eq!(source.keep, under_way, "what is under way is kept");
    tickets_open(&r, &source);

    let sb = &mut r.others[0].1;
    let kept = told_of(sb, replicas(|r| r.keeping.iter().any(|k| !k.receiving.is_empty())))
        .await
        .keeping[0]
        .clone();
    assert_eq!(
        (kept.volume.as_str(), kept.receiving.as_str(), kept.from.as_str()),
        (r.volume.as_str(), snapshot.as_str(), r.a.0.node.as_str()),
        "told what it receives, and from which node"
    );
    send(sb, vec![received(1, &r.volume, &snapshot, false)]).await;
    until(sb, |d| ack(d).filter(|n| *n == 1)).await;
    let sc = &mut r.others[1].1;
    let other = uuid::Uuid::now_v7().to_string();
    send(sc, vec![received(1, &r.volume, &other, false)]).await;
    until(sc, |d| ack(d).filter(|n| *n == 1)).await;
    assert_eq!(r.h.effects("journal.rejected").await.len(), 1, "not the snapshot sent to it");

    let shown = replicas_once(&r.h, &r.project, |s| s.iter().any(|x| x.received_at.is_set())).await;
    let fresh: Vec<&str> =
        shown.iter().filter(|x| x.received_at.is_set()).map(|x| x.node.as_str()).collect();
    assert_eq!(fresh, [b.as_str()], "its age shown from what it received");
    let sa = &mut r.a.1;
    let after =
        told_of(sa, replicas(|r| r.sending.first().is_some_and(|s| s.transfers.len() == 1)));
    let source = after.await.sending[0].clone();
    assert_eq!(source.transfers[0].to, c, "only the one not yet received");
    assert_eq!(source.keep, under_way, "the base of b's next increment, and c's under way");

    let sc = &mut r.others[1].1;
    send(sc, vec![received(2, &r.volume, &for_c, true)]).await;
    until(sc, |d| ack(d).filter(|n| *n == 2)).await;
    let sa = &mut r.a.1;
    let again = told_of(
        sa,
        replicas(|r| {
            r.sending.first().is_some_and(|s| s.transfers.iter().any(|t| t.snapshot != for_c))
        }),
    );
    let source = again.await.sending[0].clone();
    assert_eq!(source.transfers.len(), 1, "sent again to the one that failed");
    assert_eq!(source.keep.len(), 2, "the base of b's next increment, and c's new snapshot");
    let failed = r.h.effects("volume.replica_receive").await;
    assert_eq!(failed.len(), 2, "one received, one failed: {failed:?}");
}
