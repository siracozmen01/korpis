// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent's session (§4.2 of docs/02-architecture.md): welcomed, holding a verified set,
//! keeping it on disk, acknowledged, and told `InSync` when it reconnects holding it.

use crate::support::{ControlPlane, facts};
use korpis_agent::held::{HeldSet, Refused};
use korpis_agent::join::{Join, join};
use korpis_agent::session::{Event, Session};
use korpis_agent::state::StateDir;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::Receiver;

async fn until(events: &mut Receiver<Event>, pick: impl Fn(&Event) -> bool) -> Event {
    let wait = async {
        loop {
            let e = events.recv().await.unwrap();
            if pick(&e) {
                return e;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait).await.expect("the event within ten seconds")
}

#[tokio::test]
async fn the_agent_holds_its_verified_set_and_is_told_in_sync_when_it_returns() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let (token, ca) = cp.token("fra-3").await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));
    let join_request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &join_request).await.unwrap();

    let (session, link) = Session::new(identity.clone(), state.clone(), Arc::new(facts));
    let mut events = link.events.subscribe();
    let running = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    until(&mut events, |e| matches!(e, Event::Held { .. })).await;
    assert!(link.held.borrow().assignments.is_empty(), "nothing is placed here yet");

    let workload = cp.declare(&project, "mc").await;
    let Event::Held { digest } = until(&mut events, |e| matches!(e, Event::Held { .. })).await
    else {
        unreachable!()
    };
    let held = link.held.borrow().clone();
    assert!(held.assignments.contains_key(&workload), "the workload's assignment is held");
    let kept = HeldSet::load(
        &state.root().join("assignments.binpb"),
        &identity.keys.assignment_keys,
        &identity.node,
    )
    .unwrap();
    assert_eq!(kept.digest, digest, "kept on disk with its signatures");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let acked: String =
        cp.scalar("SELECT acked_set_digest FROM node WHERE acked_set_digest IS NOT NULL").await;
    assert_eq!(acked, digest, "acknowledged");

    let mut tampered = held.raw.clone();
    if let Some(b) = tampered.assignments.first_mut().and_then(|a| a.assignment.last_mut()) {
        *b ^= 1;
    }
    let refused = HeldSet::verify(&tampered, &identity.keys.assignment_keys, &identity.node);
    assert_eq!(refused.unwrap_err(), Refused::BadSignature(0), "a tampered set is not believed");

    running.abort();
    let (again, link) = Session::new(identity, state, Arc::new(facts));
    assert_eq!(link.held.borrow().digest, digest, "a restarted agent holds the set it kept");
    let mut events = link.events.subscribe();
    let _running = tokio::spawn(again.run());
    until(&mut events, |e| matches!(e, Event::InSync)).await;
}
