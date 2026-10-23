// SPDX-License-Identifier: AGPL-3.0-or-later

//! A query probe through the control plane (§5 of docs/04-runtimes.md, §5 of
//! docs/16-extensions.md): a declaration whose probe names a query protocol pins the health
//! provider that answers it, and is refused where none does; and the node running the workload
//! asks, round by round, what to send and then the verdict, which the provider decides from the
//! rounds alone. Only the node the workload is placed on is answered, only for a query probe, and
//! only through the installation the intent pinned.

use crate::agent::{Stream, TestAgent, welcome};
use crate::extensions::{DNS, push_document};
use crate::journal::{assignment, until};
use crate::providers::installed_from;
use crate::recipes::{body, create, project, push_paper};
use crate::step_resolve::only;
use crate::support::{Harness, mutation};
use korpis_extension::health::{Failure, Health, HealthProvider, send, verdict};
use korpis_extension::provider::{Provider, Registration};
use korpis_extension::{Connection, Credential, Fixed};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::health_answer::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::health_next::Next;
use korpis_proto::korpis::v1::__buffa::oneof::probe::Kind;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// `korpis-test/echo`: send "ping", and the workload is healthy if it said "pong".
struct Echo;

impl HealthProvider for Echo {
    fn step(
        &self,
        step: pb::HealthStep,
    ) -> impl Future<Output = Result<pb::HealthNext, Failure>> + Send {
        let next = match step.rounds.last() {
            None => send(b"ping", 16, Duration::from_secs(1)),
            Some(r) => verdict(r.received == b"pong", "it answers pong, or does not"),
        };
        std::future::ready(Ok(next))
    }
}

/// Installs an extension providing the `korpis-test/echo` query protocol and serves it with
/// [`Echo`] through the SDK until the test ends; returns the installation.
pub async fn serving(h: &Harness) -> pb::Extension {
    let document = DNS
        .replace(
            "    - {type: dns, name: cloudflare}",
            "    - {type: health, name: korpis-test/echo}",
        )
        .replace(
            "requires: [provider.serve, provider.dns,",
            "requires: [provider.serve, provider.health,",
        );
    let x = installed_from(h, 62, "probes", push_document(&document).await).await;
    let connection = Arc::new(Connection::new(Fixed(Credential {
        secret: x.secret.clone(),
        expires_at: SystemTime::now() + Duration::from_secs(900),
        control_plane: h.base_url(),
        ca_pem: h.ca_pem.clone(),
        relay: None,
    })));
    let registration = Registration {
        kind: pb::ProviderType::PROVIDER_TYPE_HEALTH,
        name: "korpis-test/echo".into(),
        version: "1.0.0".into(),
        concurrency: 4,
        sign_in: false,
    };
    let provider = Arc::new(Provider::new(connection, registration, Health(Echo)));
    let mut registered = provider.registered();
    let serving = Arc::clone(&provider);
    tokio::spawn(async move { serving.run().await });
    registered.wait_for(Option::is_some).await.unwrap();
    x.extension
}

/// A body of the example recipe whose liveness probe queries `protocol` on its game endpoint.
fn querying(repo: &str, protocol: &str) -> pb::IntentBody {
    let query =
        pb::QueryProbe { protocol: protocol.into(), endpoint: "game".into(), ..Default::default() };
    let probe = pb::Probe { kind: Some(Kind::Query(Box::new(query))), ..Default::default() };
    let health = pb::HealthSpec { liveness: probe.into(), ..Default::default() };
    pb::IntentBody { health: health.into(), ..body(repo, &[]) }
}

/// Asks what follows `rounds` in `probe`'s exchange; the outcome.
async fn ask(
    s: &mut Stream,
    request: &str,
    (workload, intent, probe): (&str, &str, agent::ProbeRole),
    rounds: Vec<pb::HealthRound>,
) -> Outcome {
    let up = agent::HealthQuery {
        request: request.into(),
        workload: workload.into(),
        intent: intent.into(),
        probe: probe.into(),
        rounds,
        within: buffa_types::google::protobuf::Duration { seconds: 5, ..Default::default() }.into(),
        ..Default::default()
    };
    let message =
        agent::AgentMessage { message: Some(Up::HealthQuery(Box::new(up))), ..Default::default() };
    s.send(message).await.unwrap();
    let pick = |d: &Down| match d {
        Down::HealthAnswer(a) if a.request == request => a.outcome.clone(),
        _ => None,
    };
    until(s, pick).await
}

fn round(sent: &[u8], received: &[u8]) -> pb::HealthRound {
    pb::HealthRound { sent: sent.to_vec(), received: received.to_vec(), ..Default::default() }
}

fn next(o: Outcome) -> Next {
    match o {
        Outcome::Next(n) => n.next.clone().expect("a next step"),
        Outcome::Unjudged(e) => panic!("unjudged: {e:?}"),
    }
}

fn unjudged(o: Outcome) -> String {
    match o {
        Outcome::Unjudged(e) => e.reason.clone(),
        Outcome::Next(n) => panic!("answered: {n:?}"),
    }
}

#[tokio::test]
async fn a_node_asks_its_query_probe_s_rounds_and_verdict_of_the_provider_its_intent_pinned() {
    let h = Harness::new().await;
    let echo = serving(&h).await;
    let node = TestAgent::enroll(&h, "fra-4", 1).await;
    let (mut s, first) = TestAgent::open(&h, node.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    only(&h, "fra-4").await;
    let project = project(&h, &h.root_org(), "games").await;
    let repo = push_paper().await;
    let workloads = h.workloads(h.root());
    let unanswered =
        workloads.declare_intent(create("d0", &project, "x", querying(&repo, "nobody/q")));
    let refused = unanswered.await.unwrap_err();
    assert!(format!("{refused:?}").contains("no installed extension answers"), "{refused:?}");

    let declared =
        workloads.declare_intent(create("d1", &project, "mc", querying(&repo, "korpis-test/echo")));
    let declared = declared.await.unwrap().into_owned();
    let intent = declared.intent.as_option().unwrap();
    let recipe = intent.body.as_option().unwrap().recipe.as_option().unwrap();
    let health = pb::ProviderType::PROVIDER_TYPE_HEALTH;
    let pinned = recipe.providers.iter().find(|p| p.r#type.as_known() == Some(health));
    assert_eq!(
        pinned.map(|p| (p.step.as_str(), p.extension.as_str())),
        Some(("korpis-test/echo", echo.id.as_str()))
    );
    let workload = declared.workload.as_option().unwrap().id.clone();
    until(&mut s, assignment(|a| a.plan.is_set())).await;

    let liveness = (workload.as_str(), intent.id.as_str(), agent::ProbeRole::PROBE_ROLE_LIVENESS);
    let Next::Send(first) = next(ask(&mut s, "q1", liveness, vec![]).await) else {
        panic!("a round")
    };
    assert_eq!(first.data, b"ping");
    let Next::Verdict(v) = next(ask(&mut s, "q2", liveness, vec![round(b"ping", b"pong")]).await)
    else {
        panic!("a verdict")
    };
    assert!(v.healthy, "{v:?}");
    let Next::Verdict(v) = next(ask(&mut s, "q3", liveness, vec![round(b"ping", b"")]).await)
    else {
        panic!("a verdict")
    };
    assert!(!v.healthy, "silence is not pong");

    let readiness = (liveness.0, liveness.1, agent::ProbeRole::PROBE_ROLE_READINESS);
    let tcp = ask(&mut s, "q4", readiness, vec![]).await;
    assert_eq!(unjudged(tcp), "health.not_a_query", "its readiness probe is the recipe's tcp one");
    let elsewhere = (intent.workload.as_str(), "int_00000000000000000000000000000000", liveness.2);
    assert_eq!(unjudged(ask(&mut s, "q5", elsewhere, vec![]).await), "health.not_assigned");

    let gone = pb::UninstallExtensionRequest {
        mutation: mutation("u1").into(),
        id: echo.id.clone(),
        ..Default::default()
    };
    h.extensions(h.root()).uninstall_extension(gone).await.unwrap();
    let changed = ask(&mut s, "q6", liveness, vec![]).await;
    assert_eq!(unjudged(changed), "health.provider_changed", "only what the intent pinned answers");
}
