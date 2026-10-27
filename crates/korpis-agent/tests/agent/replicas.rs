// SPDX-License-Identifier: AGPL-3.0-or-later

//! Two nodes' part in replication (§2.2 of docs/06-storage.md), each with its own mesh address
//! on the loopback, which Linux holds whole: the volume's node is told a transfer to the other,
//! and sends it from its mesh address with its ticket; the other was told it receives that
//! snapshot from that node, admits the stream, receives it, and journals that it holds it. A
//! stream under a ticket the token keys do not open is refused, and nothing of it is journaled.

use korpis_agent::host::InMemoryHost;
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::replicas::Replication;
use korpis_agent::session::Event;
use korpis_core::authz::token::{KeyRing, SigningKey};
use korpis_core::authz::transfer::{LIFETIME, Transfer, mint};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_wire::wire_id;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

/// One node: its identifier, mesh address, host, journal, and what it is told on.
struct Node {
    id: Uuid,
    host: InMemoryHost,
    journal: JournalHandle,
    events: broadcast::Sender<Event>,
    _dir: tempfile::TempDir,
}

fn node(id: Uuid, mesh: Ipv4Addr, keys: &KeyRing) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let journal = JournalHandle::open(&dir.path().join("journal"), Bounds::default()).unwrap();
    let host = InMemoryHost::new();
    let (_, ring) = watch::channel(Arc::new(keys.clone()));
    let r = Replication::new(host.clone(), (dir.path(), id, Some(mesh)), ring, journal.clone());
    let (events, _) = broadcast::channel(16);
    tokio::spawn(r.run(events.subscribe()));
    Node { id, host, journal, events, _dir: dir }
}

/// The mesh: `a` with the pool whose last address is `127.0.1.255`, `b` with `127.0.2.255`.
fn peers(a: Uuid, b: Uuid) -> agent::Peers {
    let peer = |id, pool: &str| agent::Peer {
        node: wire_id(IdKind::Node, id),
        allowed_ips: vec![pool.to_owned()],
        ..Default::default()
    };
    agent::Peers {
        peers: vec![peer(a, "127.0.1.0/24"), peer(b, "127.0.2.0/24")],
        ..Default::default()
    }
}

/// What `journal` holds of receives.
async fn receives(journal: &JournalHandle) -> Vec<agent::ReplicaEvent> {
    let records = journal.read_after(0, 1 << 20).await.unwrap();
    records
        .into_iter()
        .filter_map(|r| match r.body {
            Some(Body::Replica(e)) => Some(*e),
            _ => None,
        })
        .collect()
}

/// A transfer of `snapshot` of `volume` from `a` to `b`, signed by `key`.
fn transfer(
    key: &SigningKey,
    (volume, a, b): (Uuid, Uuid, Uuid),
    snapshot: Uuid,
) -> agent::ReplicaTransfer {
    let expires_at = SystemTime::now() + LIFETIME;
    let t = Transfer { volume, from: a, to: b, snapshot, parent: None, expires_at };
    let seconds = expires_at.duration_since(UNIX_EPOCH).unwrap().as_secs();
    agent::ReplicaTransfer {
        to: wire_id(IdKind::Node, b),
        snapshot: snapshot.to_string(),
        expires_at: buffa_types::google::protobuf::Timestamp {
            seconds: i64::try_from(seconds).unwrap(),
            ..Default::default()
        }
        .into(),
        ticket: mint(key, &t),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_snapshot_is_sent_under_its_ticket_received_and_journaled_and_a_forged_one_refused() {
    let (key, other_key) = (SigningKey::generate(1), SigningKey::generate(2));
    let mut keys = KeyRing::default();
    keys.insert(key.key_id, &key.public_bytes()).unwrap();
    let (volume, a_id, b_id) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let a = node(a_id, Ipv4Addr::new(127, 0, 1, 255), &keys);
    let b = node(b_id, Ipv4Addr::new(127, 0, 2, 255), &keys);
    let (good, forged) = (Uuid::now_v7(), Uuid::now_v7());
    let source = agent::ReplicaSource {
        volume: wire_id(IdKind::Volume, volume),
        helper_id: "wl_1.data".into(),
        keep: vec![good.to_string(), forged.to_string()],
        transfers: vec![
            transfer(&key, (volume, a_id, b_id), good),
            transfer(&other_key, (volume, a_id, b_id), forged),
        ],
        ..Default::default()
    };
    let kept = |snapshot: Uuid| agent::ReplicaKept {
        volume: wire_id(IdKind::Volume, volume),
        receiving: snapshot.to_string(),
        from: wire_id(IdKind::Node, a_id),
        storage_class: "local".into(),
        ..Default::default()
    };
    for n in [&a, &b] {
        n.events.send(Event::Peers(peers(a_id, b_id))).unwrap();
    }
    let told_b = agent::Replicas { keeping: vec![kept(good), kept(forged)], ..Default::default() };
    b.events.send(Event::Replicas(told_b)).unwrap();
    // The receiving node listens before it is sent anything.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let told_a = agent::Replicas { sending: vec![source], ..Default::default() };
    a.events.send(Event::Replicas(told_a)).unwrap();

    let mut held = Vec::new();
    for _ in 0..100 {
        held = receives(&b.journal).await;
        if !held.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let held = if held.len() < 2 { receives(&b.journal).await } else { held };
    assert_eq!(held.len(), 1, "the forged one refused: {held:?}");
    assert_eq!(held[0].snapshot, good.to_string());
    let received = Some(agent::ReplicaEventKind::REPLICA_EVENT_KIND_RECEIVED);
    assert_eq!(held[0].event.as_known(), received, "{held:?}");
    assert_eq!(held[0].volume, wire_id(IdKind::Volume, volume));
    let sent: Vec<String> = a.host.replicas_sent().into_iter().map(|r| r.volume).collect();
    assert!(sent.iter().all(|v| v == "wl_1.data") && !sent.is_empty(), "{sent:?}");
    assert!(receives(&a.journal).await.is_empty(), "the sender journals nothing of it");
    assert_ne!(a.id, b.id);
}
