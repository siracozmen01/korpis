// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list on the node (§6.3 of docs/08-identity.md): sent when the session opens,
//! sent again when a grant is revoked, believed once its signature verifies against the token
//! keys enrollment returned, and kept, so a restarted agent refuses the same chains before it
//! reaches the control plane again.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::session::{Event, Session};
use korpis_agent::state::StateDir;
use std::sync::Arc;

#[tokio::test]
async fn a_revoked_grant_reaches_the_node_and_stays_refused_across_a_restart() {
    let cp = ControlPlane::start().await;
    let (token, ca) = cp.token("fra-4").await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));
    let join_request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &join_request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let (session, link) =
        Session::new(identity.clone(), state.clone(), Arc::new(facts), journal.clone());
    let mut events = link.events.subscribe();
    let running = tokio::spawn(session.run());
    let first = until(&mut events, |e| matches!(e, Event::Revocations { .. })).await;
    assert!(matches!(first, Event::Revocations { entries: 0, .. }), "{first:?}");

    let grant = cp.revoked_grant().await;
    until(&mut events, |e| matches!(e, Event::Revocations { entries: 1, .. })).await;
    assert_eq!(link.revoked.borrow().refuses(&[grant]), Some(grant));

    running.abort();
    let (_again, link) = Session::new(identity, state, Arc::new(facts), journal);
    assert_eq!(link.revoked.borrow().refuses(&[grant]), Some(grant), "kept, and verified again");
}
