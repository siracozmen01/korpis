// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reachability measured (§9 of docs/07-networking.md): a node advertising an address the
//! control plane can reach it at, and answering there with its key, is measured public; one
//! advertising an address nobody answers at is measured, and stays edge-only.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::session::{Event, Facts, Session};
use korpis_agent::state::StateDir;
use std::sync::Arc;
use std::time::Duration;

/// Runs a node named `name` advertising `address`; its session task.
async fn node(
    cp: &ControlPlane,
    name: &str,
    address: &str,
    dir: &std::path::Path,
) -> tokio::task::JoinHandle<()> {
    let (token, ca) = cp.token(name).await;
    let state = StateDir::new(dir.join(name));
    let join_request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &join_request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let mut reported = facts();
    reported.advertised.addresses = vec![address.to_owned()];
    let reported: Arc<dyn Fn() -> Facts + Send + Sync> = Arc::new(move || reported.clone());
    let (session, link) = Session::new(identity, state, reported, journal);
    let mut events = link.events.subscribe();
    let running = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    running
}

/// Waits until `name` has been measured; its reachability.
async fn measured(cp: &ControlPlane, name: &str) -> String {
    for _ in 0..300 {
        let row: Option<(String, bool)> = cp
            .query_one("SELECT reachability, probed_at IS NOT NULL FROM node WHERE name = $1", name)
            .await;
        if let Some((reachability, true)) = row {
            return reachability;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{name} was not measured within thirty seconds");
}

#[tokio::test]
async fn a_node_is_measured_at_what_it_advertises() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let _reachable = node(&cp, "fra-8", "127.0.0.1", dir.path()).await;
    let _unreachable = node(&cp, "fra-9", "192.0.2.1", dir.path()).await;
    assert_eq!(measured(&cp, "fra-8").await, "public", "it answered with its key over both");
    assert_eq!(measured(&cp, "fra-9").await, "edge_only", "nothing answered there");
}
