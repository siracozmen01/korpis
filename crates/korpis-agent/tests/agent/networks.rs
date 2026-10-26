// SPDX-License-Identifier: AGPL-3.0-or-later

//! A running workload's network follows its assignment (§5.1 of docs/05-scheduling.md, Finding
//! 181 of docs/23-walkthroughs.md): when the control plane holds it to a spent egress budget, its
//! node asks the helper for its network again, held, without the workload being created again;
//! and lets it go again once the hold is lifted.

use crate::health::{declare, observed};
use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::network::Pools;
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Duration;

/// Waits until `host` was last asked for `w`'s network with `held` saying so of its bandwidth.
async fn asked(host: &InMemoryHost, w: &str, held: bool) {
    let cut_off = |h: &InMemoryHost| {
        h.network(w).and_then(|r| r.bandwidth.as_option().map(|b| b.cut_off)).unwrap_or(false)
    };
    for _ in 0..300 {
        if cut_off(host) == held {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w}'s network was never asked with cut_off {held}: {:?}", host.network(w));
}

#[tokio::test]
async fn a_running_workload_s_network_is_held_when_its_budget_is_spent_and_let_go_after() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("networks-1").await;
    let state = StateDir::new(dir.path().join("networks-1"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let options = Options {
        period: Duration::from_millis(200),
        network: Some(Pools { ipv4: "10.77.0.0/16".parse().unwrap(), ipv6: None }),
        ..Options::default()
    };
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let agent =
        Agent::start(&state, runtime.clone(), host.clone(), Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    cp.measured().await;
    let project = cp.project().await;
    let w = declare(&cp, &project, "held", pb::IntentBody::default()).await;
    let running =
        |o: &pb::Observation| o.state.as_known() == Some(pb::ObservedState::OBSERVED_STATE_RUNNING);
    observed(&cp, &w, running).await;
    asked(&host, &w, false).await;
    let created = runtime.objects().values().filter(|(x, _)| *x == w).count();

    // As the reckoning holds a workload of a spent budget: a row its trigger announces.
    cp.execute(
        "INSERT INTO workload_egress (workload_id, month, floor_bits_per_sec, cut_off) \
         SELECT id, date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC', NULL, true \
           FROM workload WHERE name = 'held'",
    )
    .await;
    asked(&host, &w, true).await;
    cp.execute("DELETE FROM workload_egress").await;
    asked(&host, &w, false).await;
    let now = runtime.objects().values().filter(|(x, _)| *x == w).count();
    assert_eq!(now, created, "held and let go while it ran, not created again");
    agent.stop().await;
}
