// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node journal end to end (§4.10 of docs/02-architecture.md): what the node records is
//! ingested exactly once and in order, across a restart and while disconnected, with repeats
//! coalesced; and a control plane restored from a backup older than the journal's tail is sent
//! a gap record for what the node no longer holds.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::session::{Event, Session};
use korpis_agent::state::{Identity, StateDir};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Duration;

fn glass(n: u64) -> Body {
    Body::BreakGlass(Box::new(agent::BreakGlass {
        operator: format!("operator-{n}"),
        reason: "x".repeat(1000),
        session: n.to_string(),
        ..Default::default()
    }))
}

fn drift() -> Body {
    Body::Effect(Box::new(pb::Effect {
        action: "node.drift".into(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED.into(),
        ..Default::default()
    }))
}

async fn joined(cp: &ControlPlane, dir: &tempfile::TempDir) -> (Identity, StateDir) {
    let (token, ca) = cp.token("fra-4").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    (join(&state, &request).await.unwrap(), state)
}

/// Starts a session sending `journal`, once it is welcomed.
async fn connect(
    identity: &Identity,
    state: &StateDir,
    journal: &JournalHandle,
) -> tokio::task::JoinHandle<()> {
    let (session, link) =
        Session::new(identity.clone(), state.clone(), Arc::new(facts), journal.clone());
    let mut events = link.events.subscribe();
    let running = tokio::spawn(async move {
        let _link = link;
        session.run().await;
    });
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    running
}

/// Waits until the control plane has ingested through `seq`.
async fn ingested(cp: &ControlPlane, seq: i64) {
    for _ in 0..200 {
        let through: Option<i64> =
            cp.scalar("SELECT max(ingested_through) FROM node_journal_cursor").await;
        if through == Some(seq) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the control plane did not ingest through {seq}");
}

#[tokio::test]
async fn the_journal_is_ingested_once_in_order_across_a_restart_with_repeats_coalesced() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (identity, state) = joined(&cp, &dir).await;
    let bounds = Bounds { coalesce: Duration::from_millis(300), ..Bounds::default() };
    let journal = JournalHandle::open(&state.root().join("journal"), bounds).unwrap();
    let running = connect(&identity, &state, &journal).await;
    for body in [glass(1), glass(2), drift()] {
        journal.append(body).await.unwrap();
    }
    ingested(&cp, 3).await;
    running.abort();

    // Disconnected: two more, then a run of identical effects coalesced into two records.
    journal.append(glass(3)).await.unwrap();
    journal.append(glass(4)).await.unwrap();
    for _ in 0..4 {
        journal.append(drift()).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(350)).await;
    journal.maintain().await.unwrap();
    assert_eq!(journal.marks().await.unwrap(), (7, 1), "seven records, all still held");

    let _running = connect(&identity, &state, &journal).await;
    ingested(&cp, 7).await;
    let rows: i64 = cp.scalar("SELECT count(*) FROM effect WHERE origin_seq IS NOT NULL").await;
    let distinct: i64 = cp
        .scalar("SELECT count(DISTINCT origin_seq) FROM effect WHERE origin_seq IS NOT NULL")
        .await;
    assert_eq!((rows, distinct), (7, 7), "exactly once");
    let glass_rows: i64 =
        cp.scalar("SELECT count(*) FROM effect WHERE action = 'node.break_glass'").await;
    assert_eq!(glass_rows, 4);
    let repeats: i64 = cp.scalar("SELECT count FROM effect WHERE origin_seq = 7").await;
    assert_eq!(repeats, 3, "the first drift of the run was written alone, then three repeats");
}

#[tokio::test]
async fn a_control_plane_restored_from_an_older_backup_is_sent_a_gap_for_what_was_discarded() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (identity, state) = joined(&cp, &dir).await;
    let bounds = Bounds {
        bytes: 400 << 10,
        segment: 64 << 10,
        retention: Duration::ZERO,
        ..Bounds::default()
    };
    let journal = JournalHandle::open(&state.root().join("journal"), bounds).unwrap();
    let running = connect(&identity, &state, &journal).await;
    for n in 1..=150 {
        journal.append(glass(n)).await.unwrap();
    }
    ingested(&cp, 150).await;
    journal.maintain().await.unwrap();
    let (head, tail) = journal.marks().await.unwrap();
    assert!(head == 150 && tail > 11, "acknowledged segments were discarded: tail {tail}");
    running.abort();

    cp.execute("UPDATE node_journal_cursor SET ingested_through = 10").await;
    let _running = connect(&identity, &state, &journal).await;
    ingested(&cp, 150).await;
    let gaps: i64 = cp.scalar("SELECT count(*) FROM effect WHERE action = 'journal.gap'").await;
    assert_eq!(gaps, 1, "one gap names what the node no longer holds");
    let first: i64 = cp
        .scalar("SELECT (error->>'first')::bigint FROM effect WHERE action = 'journal.gap'")
        .await;
    assert_eq!(first, 11);
}
