// SPDX-License-Identifier: AGPL-3.0-or-later

//! A heartbeat probe run by the agent (§5 of docs/04-runtimes.md): the workload says on its
//! identity socket that it is alive, as the SDK and `korpis heartbeat` do, and the agent judges
//! its silence. Healthy while it beats; once it has been silent longer than its probe allows,
//! unhealthy and restarted for it, recorded with its cause; and healthy again once it beats in
//! its new life.

use crate::health::{declare, health, observed};
use crate::session::until;
use crate::support::{ControlPlane, facts};
use buffa_types::google::protobuf::Duration as PbDuration;
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_extension::IdentitySocket;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::probe::Kind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// An agent joined to `cp` that serves its workloads identity sockets under `dir`; the agent,
/// and the sockets' root.
async fn agent(cp: &ControlPlane, dir: &Path) -> (Agent, PathBuf) {
    let (token, ca) = cp.token("beating-1").await;
    let state = StateDir::new(dir.join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let root = dir.join("identity");
    let options = Options {
        period: Duration::from_millis(200),
        identity_root: Some(root.clone()),
        ..Options::default()
    };
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let agent = Agent::start(&state, runtime, host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    cp.measured().await;
    (agent, root)
}

/// A liveness probe that allows two seconds of silence, checked every second.
fn heartbeat() -> pb::HealthSpec {
    let seconds = |seconds| PbDuration { seconds, ..Default::default() };
    let beat = pb::HeartbeatProbe { max_interval: seconds(2).into(), ..Default::default() };
    let probe = pb::Probe {
        kind: Some(Kind::Heartbeat(Box::new(beat))),
        period: seconds(1).into(),
        failures: 1,
        ..Default::default()
    };
    pb::HealthSpec { liveness: probe.into(), ..Default::default() }
}

/// Beats on `socket` every 300 ms while `beating`, as a workload would.
async fn beat(socket: PathBuf, beating: Arc<AtomicBool>) {
    let socket = IdentitySocket::at(socket);
    loop {
        if beating.load(Ordering::Relaxed) {
            // The socket is bound again when the workload restarts; a beat meanwhile is lost.
            let _ = socket.heartbeat().await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[tokio::test]
async fn a_workload_silent_longer_than_its_heartbeat_allows_is_unhealthy_and_restarted() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (agent, root) = agent(&cp, dir.path()).await;
    let project = cp.project().await;
    let body = pb::IntentBody { health: heartbeat().into(), ..Default::default() };
    let w = declare(&cp, &project, "bot", body).await;
    let beating = Arc::new(AtomicBool::new(true));
    tokio::spawn(beat(root.join(&w).join("identity.sock"), Arc::clone(&beating)));

    let healthy = |o: &pb::Observation| health(o) == Some(pb::HealthState::HEALTH_STATE_HEALTHY);
    observed(&cp, &w, healthy).await;
    beating.store(false, Ordering::Relaxed);
    let unhealthy = pb::HealthState::HEALTH_STATE_UNHEALTHY;
    observed(&cp, &w, |o| health(o) == Some(unhealthy)).await;
    let restarted = "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                     AND after::text LIKE '%unhealthy%' AND after::text LIKE '%heartbeat%'";
    for _ in 0..100 {
        if cp.scalar::<i64>(restarted).await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cp.scalar::<i64>(restarted).await >= 1, "the restart is recorded with its cause");
    beating.store(true, Ordering::Relaxed);
    observed(&cp, &w, healthy).await;
    agent.stop().await;
}
