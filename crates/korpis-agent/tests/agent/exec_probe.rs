// SPDX-License-Identifier: AGPL-3.0-or-later

//! An exec probe run by the agent (§5 of docs/04-runtimes.md), with the in-memory runtime
//! standing in for a driver that runs commands: healthy while the command succeeds; once it
//! fails, unhealthy and restarted for it, recorded with its cause; and healthy again once it
//! succeeds. The command runs inside the workload's object, through its driver.

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

/// An agent joined to `cp` over the in-memory runtime; the agent, and its runtime.
async fn agent(cp: &ControlPlane, dir: &tempfile::TempDir) -> (Agent, InMemory) {
    let (token, ca) = cp.token("execing-1").await;
    let state = StateDir::new(dir.path().join("execing-1"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    cp.measured().await;
    (agent, runtime)
}

/// A liveness probe that runs `pg_isready` every second, failing at its first failure.
fn exec() -> pb::HealthSpec {
    let command = pb::ExecProbe { command: vec!["pg_isready".into()], ..Default::default() };
    let probe = pb::Probe {
        kind: Some(Kind::Exec(Box::new(command))),
        period: PbDuration { seconds: 1, ..Default::default() }.into(),
        failures: 1,
        ..Default::default()
    };
    pb::HealthSpec { liveness: probe.into(), ..Default::default() }
}

#[tokio::test]
async fn a_failing_command_is_unhealthy_and_restarts_its_workload() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (agent, runtime) = agent(&cp, &dir).await;
    let project = cp.project().await;
    let body = pb::IntentBody { health: exec().into(), ..Default::default() };
    let w = declare(&cp, &project, "db", body).await;

    let healthy = |o: &pb::Observation| health(o) == Some(pb::HealthState::HEALTH_STATE_HEALTHY);
    observed(&cp, &w, healthy).await;
    assert!(runtime.calls().iter().any(|c| c.starts_with("exec ")), "run through its driver");
    runtime.exec_status(1);
    let unhealthy = pb::HealthState::HEALTH_STATE_UNHEALTHY;
    observed(&cp, &w, |o| health(o) == Some(unhealthy)).await;
    let restarted = "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                     AND after::text LIKE '%unhealthy%' AND after::text LIKE '%exec%'";
    for _ in 0..100 {
        if cp.scalar::<i64>(restarted).await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cp.scalar::<i64>(restarted).await >= 1, "the restart is recorded with its cause");
    runtime.exec_status(0);
    observed(&cp, &w, healthy).await;
    agent.stop().await;
}
