// SPDX-License-Identifier: AGPL-3.0-or-later

//! Health probes run by the agent (Rule K-16, §5 of docs/04-runtimes.md), with the in-memory
//! runtime standing in for a driver and a server on this machine standing in for the workload:
//! its addresses come from a loopback pool, which the server listens on, and the in-memory host
//! opens the probe's socket here. A dependent waits until its database's readiness probe passes
//! (§5 of docs/07-networking.md); the database's health is its liveness probe's verdict; and a
//! liveness probe that fails restarts it, recorded with its cause.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation, push_paper};
use buffa_types::google::protobuf::Duration as PbDuration;
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::network::Pools;
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target as DependsOn;
use korpis_proto::korpis::v1::__buffa::oneof::probe::Kind;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// What the stand-in server says: whether it is ready, and whether it is alive.
#[derive(Default)]
struct Says {
    ready: AtomicBool,
    dead: AtomicBool,
}

/// A server on every local address, answering `/ready` and `/alive` as `says` has it; its port.
async fn serving(says: Arc<Says>) -> u16 {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut asked = [0_u8; 512];
            let n = s.read(&mut asked).await.unwrap_or(0);
            let asked = String::from_utf8_lossy(&asked[..n]).into_owned();
            let ok = if asked.starts_with("GET /ready ") {
                says.ready.load(Ordering::Relaxed)
            } else {
                asked.starts_with("GET /alive ") && !says.dead.load(Ordering::Relaxed)
            };
            let status = if ok { "200 OK" } else { "503 Unavailable" };
            let _ = s.write_all(format!("HTTP/1.0 {status}\r\n\r\n").as_bytes()).await;
        }
    });
    port
}

fn probe(path: &str, failures: u32) -> pb::Probe {
    let http = pb::HttpProbe { endpoint: "probe".into(), path: path.into(), ..Default::default() };
    pb::Probe {
        kind: Some(Kind::Http(Box::new(http))),
        period: PbDuration { seconds: 1, ..Default::default() }.into(),
        failures,
        ..Default::default()
    }
}

/// Declares `name` from the example recipe with `body`'s parts; its wire id.
async fn declare(cp: &ControlPlane, project: &str, name: &str, body: pb::IntentBody) -> String {
    let recipe =
        pb::RecipeRef { name: push_paper().await, version: "1.21.4".into(), ..Default::default() };
    let request = pb::DeclareIntentRequest {
        mutation: mutation(&format!("declare-{name}")).into(),
        target: Some(Target::Create(Box::new(pb::NewWorkload {
            project: project.to_owned(),
            name: name.into(),
            ..Default::default()
        }))),
        body: pb::IntentBody { recipe: recipe.into(), ..body }.into(),
        ..Default::default()
    };
    let declared = cp.workloads().declare_intent(request).await.unwrap().into_owned();
    declared.workload.as_option().unwrap().id.clone()
}

/// What the node last observed of `w`, once `pick` holds of it.
async fn observed(
    cp: &ControlPlane,
    w: &str,
    pick: impl Fn(&pb::Observation) -> bool,
) -> pb::Observation {
    for _ in 0..300 {
        let s = cp.status(w).await;
        if let Some(o) = s.observation.as_option().filter(|o| pick(o)) {
            return o.clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w} never reached the observation awaited: {:?}", cp.status(w).await);
}

fn health(o: &pb::Observation) -> Option<pb::HealthState> {
    o.health.as_known()
}

/// An agent joined to `cp` that gives its workloads addresses from a loopback pool, which the
/// in-memory host probes from this machine; the agent, and its runtime.
async fn agent(cp: &ControlPlane, dir: &tempfile::TempDir) -> (Agent, InMemory) {
    let (token, ca) = cp.token("probing-1").await;
    let state = StateDir::new(dir.path().join("probing-1"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let options = Options {
        period: Duration::from_millis(200),
        network: Some(Pools { ipv4: "127.78.0.0/16".parse().unwrap(), ipv6: None }),
        ..Options::default()
    };
    let runtime = InMemory::new();
    let host = InMemoryHost::new();
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    cp.measured().await;
    (agent, runtime)
}

/// A database whose endpoint `probe` is the server on `port`, probed for readiness at `/ready`
/// and for liveness at `/alive`, and a web application that requires it; their wire ids.
async fn declared(cp: &ControlPlane, port: u16) -> (String, String) {
    let project = cp.project().await;
    let endpoint = pb::EndpointSpec {
        name: "probe".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP.into(),
        container_port: u32::from(port),
        exposure: pb::Exposure::EXPOSURE_NONE.into(),
        ..Default::default()
    };
    let checks = pb::HealthSpec {
        readiness: probe("/ready", 1).into(),
        liveness: probe("/alive", 2).into(),
        ..Default::default()
    };
    let body =
        pb::IntentBody { endpoints: vec![endpoint], health: checks.into(), ..Default::default() };
    let db = declare(cp, &project, "db", body).await;
    // The recipe's endpoints are the database's too, so the dependency names the one probed,
    // once it is placed.
    let mut probed = None;
    for _ in 0..300 {
        let status = cp.status(&db).await;
        probed = status.endpoints.iter().find(|e| e.name == "probe").cloned();
        if probed.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let probed = probed.expect("the probed endpoint is placed");
    let dependency = pb::Dependency {
        name: "db".into(),
        target: Some(DependsOn::Endpoint(probed.id.clone())),
        required: true,
        ..Default::default()
    };
    let body = pb::IntentBody { dependencies: vec![dependency], ..Default::default() };
    let web = declare(cp, &project, "web", body).await;
    (db, web)
}

#[tokio::test]
async fn readiness_holds_a_dependent_back_and_a_failing_liveness_probe_restarts_its_workload() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (agent, runtime) = agent(&cp, &dir).await;
    let says = Arc::new(Says::default());
    let port = serving(Arc::clone(&says)).await;
    let (db, web) = Box::pin(declared(&cp, port)).await;

    // Alive and not ready: healthy, and its dependent waits for it.
    observed(&cp, &db, |o| health(o) == Some(pb::HealthState::HEALTH_STATE_HEALTHY)).await;
    let waiting = observed(&cp, &web, |o| o.reason_detail.contains("db")).await;
    assert_eq!(waiting.state.as_known(), Some(pb::ObservedState::OBSERVED_STATE_STARTING));
    let web_objects = || runtime.objects().values().filter(|(w, _)| *w == web).count();
    assert_eq!(web_objects(), 0, "not created while its database is not ready");

    says.ready.store(true, Ordering::Relaxed);
    let running =
        |o: &pb::Observation| o.state.as_known() == Some(pb::ObservedState::OBSERVED_STATE_RUNNING);
    observed(&cp, &web, running).await;

    // No longer alive: unhealthy, and restarted for it.
    says.dead.store(true, Ordering::Relaxed);
    observed(&cp, &db, |o| health(o) == Some(pb::HealthState::HEALTH_STATE_UNHEALTHY)).await;
    let restarted = "SELECT count(*) FROM effect WHERE action = 'node.restart' \
                     AND after::text LIKE '%unhealthy%' AND after::text LIKE '%http%'";
    for _ in 0..100 {
        if cp.scalar::<i64>(restarted).await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cp.scalar::<i64>(restarted).await >= 1, "the restart is recorded with its cause");
    says.dead.store(false, Ordering::Relaxed);
    observed(&cp, &db, |o| running(o) && health(o) == Some(pb::HealthState::HEALTH_STATE_HEALTHY))
        .await;
    agent.stop().await;
}
