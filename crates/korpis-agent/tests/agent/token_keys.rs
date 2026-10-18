// SPDX-License-Identifier: AGPL-3.0-or-later

//! The token keys a running node verifies with (§8 of docs/03-state.md): sent as every session
//! opens, kept, and replaced, not added to, when a restore has retired the ones it held.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use buffa::Message as _;
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::session::{Event, Session};
use korpis_agent::state::StateDir;
use korpis_core::authz::token::key_name;
use korpis_proto::korpis::agent::v1 as agent;
use std::sync::Arc;
use std::time::Duration;

/// The key names kept in the node's state, once they are `want`.
async fn kept(state: &StateDir, want: &[String]) -> Vec<String> {
    let path = state.root().join("token_keys.binpb");
    let mut names = Vec::new();
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(&path) {
            let keys = agent::TokenKeys::decode_from_slice(&bytes).unwrap();
            names = keys.keys.into_iter().map(|k| k.key_id).collect();
            if names == want {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    names
}

#[tokio::test]
async fn a_node_keeps_the_token_keys_in_force_and_drops_one_a_restore_retired() {
    let cp = ControlPlane::start().await;
    let (token, ca) = cp.token("fra-8").await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let (session, link) =
        Session::new(identity.clone(), state.clone(), Arc::new(facts), journal.clone());
    let mut events = link.events.subscribe();
    let first = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    assert_eq!(kept(&state, &[key_name(1)]).await, [key_name(1)], "sent and kept");
    first.abort();

    // A restore's rotation, as the store records it: a new key, and the old one retired.
    cp.execute(
        "INSERT INTO signing_key (key_id, purpose, public_key)
         VALUES ('token-2', 'token', decode(repeat('ab', 32), 'hex'))",
    )
    .await;
    cp.execute("UPDATE signing_key SET retired_at = now() WHERE key_id = 'token-1'").await;
    let (session, link) = Session::new(identity, state.clone(), Arc::new(facts), journal);
    let mut events = link.events.subscribe();
    let _second = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    assert_eq!(kept(&state, &[key_name(2)]).await, [key_name(2)], "replaced, not added to");
}
