// SPDX-License-Identifier: AGPL-3.0-or-later

//! The probes the agent makes through a workload's driver (§5 of docs/04-runtimes.md), with the
//! in-memory runtime standing in for a driver that runs commands and reaches its guests' agents:
//! an `exec` probe, healthy while its command succeeds, unhealthy and restarted once it fails,
//! recorded with its cause, and healthy again once it succeeds; and a `guest_agent` probe, the
//! same while the guest's agent answers and once it falls silent.

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
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::probe::Kind;
use std::sync::Arc;
use std::time::Duration;

/// What the node reports: the test node's driver, which runs commands, said to reach its guests'
/// agents too, as the in-memory runtime does.
fn reached() -> korpis_agent::session::Facts {
    let mut f = facts();
    for d in &mut f.drivers {
        if let Some(c) = d.capabilities.as_option_mut() {
            c.guest_agent = true;
        }
    }
    f
}

/// An agent joined to `cp` over the in-memory runtime; the agent, and its runtime.
async fn agent(cp: &ControlPlane, dir: &tempfile::TempDir) -> (Agent, InMemory) {
    let (token, ca) = cp.token("execing-1").await;
    let state = StateDir::new(dir.path().join("execing-1"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(reached), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    cp.measured().await;
    (agent, runtime)
}

/// A liveness probe of `kind`, made every second, failing at its first failure.
fn liveness(kind: Kind) -> pb::HealthSpec {
    let probe = pb::Probe {
        kind: Some(kind),
        period: PbDuration { seconds: 1, ..Default::default() }.into(),
        failures: 1,
        ..Default::default()
    };
    pb::HealthSpec { liveness: probe.into(), ..Default::default() }
}

/// Waits until `cp` records a restart that `counted`, a count of `node.restart` effects, sees.
async fn restarted_for(cp: &ControlPlane, counted: &'static str) {
    for _ in 0..100 {
        if cp.scalar::<i64>(counted).await >= 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no restart recorded: {counted}");
}

#[tokio::test]
async fn a_failing_command_is_unhealthy_and_restarts_its_workload() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (agent, runtime) = agent(&cp, &dir).await;
    let project = cp.project().await;
    let command = pb::ExecProbe { command: vec!["pg_isready".into()], ..Default::default() };
    let health_spec = liveness(Kind::Exec(Box::new(command)));
    let body = pb::IntentBody { health: health_spec.into(), ..Default::default() };
    let w = declare(&cp, &project, "db", body).await;

    let healthy = |o: &pb::Observation| health(o) == Some(pb::HealthState::HEALTH_STATE_HEALTHY);
    observed(&cp, &w, healthy).await;
    assert!(runtime.calls().iter().any(|c| c.starts_with("exec ")), "run through its driver");
    runtime.exec_status(1);
    let unhealthy = pb::HealthState::HEALTH_STATE_UNHEALTHY;
    observed(&cp, &w, |o| health(o) == Some(unhealthy)).await;
    restarted_for(
        &cp,
        "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                       AND after::text LIKE '%unhealthy%' AND after::text LIKE '%\"exec\"%'",
    )
    .await;
    runtime.exec_status(0);
    observed(&cp, &w, healthy).await;
    agent.stop().await;
}

#[tokio::test]
async fn a_guest_whose_agent_falls_silent_is_unhealthy_and_restarted() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (agent, runtime) = agent(&cp, &dir).await;
    let project = cp.project().await;
    let body = pb::IntentBody {
        health: liveness(Kind::GuestAgent(Box::default())).into(),
        ..Default::default()
    };
    let w = declare(&cp, &project, "build", body).await;

    let healthy = |o: &pb::Observation| health(o) == Some(pb::HealthState::HEALTH_STATE_HEALTHY);
    observed(&cp, &w, healthy).await;
    assert!(runtime.calls().iter().any(|c| c.starts_with("ping_guest ")), "asked of its driver");
    runtime.guest_silent(true);
    let unhealthy = pb::HealthState::HEALTH_STATE_UNHEALTHY;
    observed(&cp, &w, |o| health(o) == Some(unhealthy)).await;
    restarted_for(
        &cp,
        "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                       AND after::text LIKE '%unhealthy%' AND after::text LIKE '%guest_agent%'",
    )
    .await;
    runtime.guest_silent(false);
    observed(&cp, &w, healthy).await;
    agent.stop().await;
}
