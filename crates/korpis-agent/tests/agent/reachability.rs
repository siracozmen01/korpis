// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reachability measured (§9 of docs/07-networking.md): a node advertising an address the
//! control plane can reach it at, and answering there with its key, is measured public; one
//! advertising an address nobody answers at is measured, and stays edge-only; and a port of its
//! range something else on its machine holds is replaced by another at once.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::session::{Event, Facts, Session};
use korpis_agent::state::StateDir;
use std::sync::Arc;
use std::time::Duration;

/// Runs a node named `name` advertising `address` and the ports `range`; its session task.
async fn node(
    cp: &ControlPlane,
    (name, address, range): (&str, &str, (u32, u32)),
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
    (reported.advertised.first_port, reported.advertised.last_port) = range;
    let reported: Arc<dyn Fn() -> Facts + Send + Sync> = Arc::new(move || reported.clone());
    let (session, link) = Session::new(identity, state, reported, journal);
    let mut events = link.events.subscribe();
    let running = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    running
}

/// The range every node of these tests advertises unless it says otherwise.
const RANGE: (u32, u32) = (30_000, 32_767);

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
    let _reachable = node(&cp, ("fra-8", "127.0.0.1", RANGE), dir.path()).await;
    let _unreachable = node(&cp, ("fra-9", "192.0.2.1", RANGE), dir.path()).await;
    assert_eq!(measured(&cp, "fra-8").await, "public", "it answered with its key over both");
    assert_eq!(measured(&cp, "fra-9").await, "edge_only", "nothing answered there");
}

/// Two ports in a row, the first held here over TCP on every address, the second free when
/// looked at: a range the node can listen on only at its second port. Both are below the
/// kernel's ephemeral range, so nothing else on the machine is given the second by binding port
/// zero between the look and the node's listening, as a test running beside this one would.
fn one_held() -> (std::net::TcpListener, (u32, u32)) {
    let ephemeral = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|r| r.split_whitespace().next()?.parse::<u16>().ok())
        .unwrap_or(32_768);
    let start = 20_000 + u16::try_from(std::process::id() % 4_000).unwrap_or(0) * 2;
    (start..ephemeral.saturating_sub(1))
        .step_by(2)
        .find_map(|port| {
            let held = std::net::TcpListener::bind(("::", port)).ok()?;
            std::net::TcpListener::bind(("::", port + 1)).ok()?;
            Some((held, (u32::from(port), u32::from(port + 1))))
        })
        .expect("two free ports in a row below the ephemeral range")
}

#[tokio::test]
async fn a_port_held_on_the_node_s_machine_is_replaced_by_the_next_at_once() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (_held, range) = one_held();
    let _node = node(&cp, ("fra-10", "127.0.0.1", range), dir.path()).await;
    // Within the retry the prober waits after an attempt that measured nothing.
    assert_eq!(measured(&cp, "fra-10").await, "public", "measured at the port it could listen on");
}
