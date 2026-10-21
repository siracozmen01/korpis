// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's meter, wired as `korpis-agent run` wires it (§2 of docs/15-observability.md): at
//! the end of every metering interval the node journals a sample for each workload it holds,
//! which reaches the control plane in order like every journal record.

use crate::console_write::running;
use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::meter::Metering;
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use std::sync::Arc;
use std::time::Duration;

/// How many metering samples have reached the control plane, once `at_least` have.
async fn arrived(cp: &ControlPlane, at_least: i64) -> i64 {
    let sql = "SELECT count(*) FROM effect WHERE action = 'journal.unsupported' \
               AND after->>'kind' = $1";
    let mut seen = 0;
    for _ in 0..300 {
        seen = cp.query_one::<(i64,)>(sql, "metering").await.map_or(0, |(n,)| n);
        if seen >= at_least {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    seen
}

#[tokio::test]
async fn every_held_workload_is_sampled_at_the_end_of_every_interval() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("fra-13").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let metering = Metering { interval: Duration::from_secs(2), net: dir.path().join("net") };
    let options = Options { period: Duration::from_millis(200), metering, ..Options::default() };
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    let w = cp.declare(&cp.project().await, "mc").await;
    running(&runtime, &w).await;

    assert!(arrived(&cp, 2).await >= 2, "a sample an interval, journaled and delivered");
    assert!(dir.path().join("agent").join("meter").exists(), "the meter keeps its accounts");
    agent.stop().await;
}
