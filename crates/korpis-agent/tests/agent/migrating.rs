// SPDX-License-Identifier: AGPL-3.0-or-later

//! Two nodes' part in a migration's copies (§8.1 of docs/05-scheduling.md), each with its own
//! mesh address on the loopback: the source's role names a pass of a volume crossing by its tree,
//! with its ticket, and the source sends it from its mesh address to the destination's transfer
//! port and journals it; the destination's role names the same pass from that source, so it
//! admits the stream, receives it into its copy, and journals that it holds the pass under the
//! copy's identifier, with the bytes that crossed. A pass under a ticket the token keys do not
//! open is refused, and nothing of it is journaled.

use korpis_agent::held::HeldSet;
use korpis_agent::host::InMemoryHost;
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::migrate::Migrating;
use korpis_agent::replicas::Replication;
use korpis_agent::session::Event;
use korpis_core::authz::migration_pass::{LIFETIME, Pass, mint};
use korpis_core::authz::token::{KeyRing, SigningKey};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::wire_id;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

/// One node: its host, journal, what it holds, and what it is told on.
struct Node {
    host: InMemoryHost,
    journal: JournalHandle,
    held: watch::Sender<HeldSet>,
    events: broadcast::Sender<Event>,
    _dir: tempfile::TempDir,
}

fn node(id: Uuid, mesh: Ipv4Addr, keys: &KeyRing) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let journal = JournalHandle::open(&dir.path().join("journal"), Bounds::default()).unwrap();
    let host = InMemoryHost::new();
    let (_, ring) = watch::channel(Arc::new(keys.clone()));
    let (held, watched) = watch::channel(HeldSet::default());
    let (events, _) = broadcast::channel(16);
    let (passes, incoming) = tokio::sync::mpsc::channel(4);
    let at = (dir.path(), id, Some(mesh));
    let r = Replication::new(host.clone(), at, ring.clone(), (journal.clone(), Some(passes)));
    let m = Migrating::new(host.clone(), at, (ring, watched), journal.clone());
    tokio::spawn(r.run(events.subscribe()));
    tokio::spawn(m.run(events.subscribe(), incoming));
    Node { host, journal, held, events, _dir: dir }
}

/// The mesh: `a` with the pool whose last address is `127.0.3.255`, `b` with `127.0.4.255`.
fn peers(a: Uuid, b: Uuid) -> agent::Peers {
    let peer = |id, pool: &str| agent::Peer {
        node: wire_id(IdKind::Node, id),
        allowed_ips: vec![pool.to_owned()],
        ..Default::default()
    };
    agent::Peers {
        peers: vec![peer(a, "127.0.3.0/24"), peer(b, "127.0.4.0/24")],
        ..Default::default()
    }
}

/// What `journal` holds of migrations' reports.
async fn reports(journal: &JournalHandle) -> Vec<agent::MigrationProgress> {
    let records = journal.read_after(0, 1 << 20).await.unwrap();
    records
        .into_iter()
        .filter_map(|r| match r.body {
            Some(Body::Migration(p)) => Some(*p),
            _ => None,
        })
        .collect()
}

/// The set holding workload `wl_m` migrating as `side` with `peer`, its volume `data` crossing
/// by its tree in the pass `pass`, under `ticket`.
fn holding(
    side: agent::MigrationSide,
    (peer, migration): (Uuid, Uuid),
    pass: (Uuid, &str),
) -> HeldSet {
    let volume = agent::MigrationVolumeRole {
        name: "data".into(),
        source_helper_id: "wl_m.data".into(),
        crossing: pb::MigrationCrossing::MIGRATION_CROSSING_TREE.into(),
        volume: wire_id(IdKind::Volume, Uuid::from_u128(9)),
        pass: agent::MigrationPass {
            pass: pass.0.to_string(),
            ticket: pass.1.to_owned(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    let role = agent::MigrationRole {
        role: side.into(),
        phase: agent::MigrationPhase::MIGRATION_PHASE_REPLICATE.into(),
        peer: wire_id(IdKind::Node, peer),
        migration: wire_id(IdKind::Migration, migration),
        volumes: vec![volume],
        ..Default::default()
    };
    let a =
        agent::Assignment { workload: "wl_m".into(), migration: role.into(), ..Default::default() };
    let mut held = HeldSet::default();
    held.assignments.insert("wl_m".into(), a);
    held
}

/// The ticket of `pass` from `a` to `b` of `migration`, signed by `key`.
fn ticket(key: &SigningKey, (migration, a, b): (Uuid, Uuid, Uuid), pass: Uuid) -> String {
    let p = Pass {
        migration,
        volume: Uuid::from_u128(9),
        from: a,
        to: b,
        pass,
        parent: None,
        expires_at: SystemTime::now() + LIFETIME,
    };
    mint(key, &p)
}

/// Waits until the transfer port at `mesh` accepts connections.
async fn listening(mesh: Ipv4Addr) {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect((mesh, korpis_agent::replicas::TRANSFER_PORT))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the transfer port never listened");
}

/// Waits until `journal` holds `n` reports.
async fn reported(journal: &JournalHandle, n: usize) -> Vec<agent::MigrationProgress> {
    for _ in 0..200 {
        let got = reports(journal).await;
        if got.len() >= n {
            return got;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    reports(journal).await
}

#[tokio::test]
async fn a_pass_is_sent_under_its_ticket_received_into_the_copy_and_a_forged_one_refused() {
    use agent::MigrationSide as S;
    let (key, other) = (SigningKey::generate(1), SigningKey::generate(2));
    let mut keys = KeyRing::default();
    keys.insert(key.key_id, &key.public_bytes()).unwrap();
    let (a, b, m) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let source = node(a, Ipv4Addr::new(127, 0, 3, 255), &keys);
    let destination = node(b, Ipv4Addr::new(127, 0, 4, 255), &keys);
    for n in [&source, &destination] {
        n.events.send(Event::Peers(peers(a, b))).unwrap();
    }

    listening(Ipv4Addr::new(127, 0, 4, 255)).await;
    let pass = Uuid::now_v7();
    let good = ticket(&key, (m, a, b), pass);
    destination.held.send(holding(S::MIGRATION_SIDE_DESTINATION, (a, m), (pass, &good))).unwrap();
    source.held.send(holding(S::MIGRATION_SIDE_SOURCE, (b, m), (pass, &good))).unwrap();
    let [sent] = reported(&source.journal, 1).await.try_into().unwrap();
    assert_eq!(sent.volumes[0].pass, pass.to_string());
    let [held] = reported(&destination.journal, 1).await.try_into().unwrap();
    let part = &held.volumes[0];
    assert!(part.held && part.helper_id == "wl_m.data", "{part:?}");
    assert!(part.sent_bytes > 0, "a tree's bytes are counted where it is pulled");
    assert_eq!(held.side.as_known(), Some(S::MIGRATION_SIDE_DESTINATION));
    let got = destination.host.tree_received("wl_m.data", &pass.to_string()).unwrap();
    assert_eq!(String::from_utf8(got).unwrap(), format!("tree wl_m.data pass {pass}\n"));

    let forged_pass = Uuid::now_v7();
    let forged = ticket(&other, (m, a, b), forged_pass);
    let both = |side| {
        holding(
            side,
            (if side == S::MIGRATION_SIDE_SOURCE { b } else { a }, m),
            (forged_pass, &forged),
        )
    };
    destination.held.send(both(S::MIGRATION_SIDE_DESTINATION)).unwrap();
    source.held.send(both(S::MIGRATION_SIDE_SOURCE)).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(reports(&destination.journal).await.len(), 1, "nothing of it is held");
    assert!(destination.host.tree_received("wl_m.data", &forged_pass.to_string()).is_none());
}
