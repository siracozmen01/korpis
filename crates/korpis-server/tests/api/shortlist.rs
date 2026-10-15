// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shortlist query (§2.1 of docs/05-scheduling.md): before a workload is bound, the best
//! scored nodes are asked whether their driver accepts it. A refusal passes the node over and is
//! kept for that target; a node that does not answer in time is passed over for that bind alone;
//! and an agent that does not say it answers is not asked at all.

use crate::agent::{Stream, TestAgent, welcome};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// How a test node answers the queries it is sent.
struct Answering {
    /// Whether it answers at all.
    answers: AtomicBool,
    /// Whether its driver accepts.
    accepts: bool,
    /// How many queries it was sent.
    asked: AtomicUsize,
}

impl Answering {
    fn new(accepts: bool) -> Arc<Self> {
        Arc::new(Self { answers: AtomicBool::new(true), accepts, asked: AtomicUsize::new(0) })
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    /// Its answer to `q`.
    fn result(&self, q: &agent::FeasibilityQuery) -> agent::AgentMessage {
        let result = agent::FeasibilityResult {
            query: q.query.clone(),
            accepted: self.accepts,
            reason: pb::UnsatisfiableReason::UNSATISFIABLE_REASON_PREPARE_REJECTED.into(),
            detail: if self.accepts { String::new() } else { "no such seccomp profile".into() },
            ..Default::default()
        };
        let up = Up::FeasibilityResult(Box::new(result));
        agent::AgentMessage { message: Some(up), ..Default::default() }
    }
}

/// Answers every query on `s` as `how` says, until the stream ends.
fn answer(mut s: Stream, how: Arc<Answering>) {
    tokio::spawn(async move {
        while let Ok(Some(m)) = s.message::<agent::ControlMessage>().await {
            let Some(Down::FeasibilityQuery(q)) = m.to_owned_message().message else { continue };
            how.asked.fetch_add(1, Ordering::SeqCst);
            if how.answers.load(Ordering::SeqCst) && s.send(how.result(&q)).await.is_err() {
                return;
            }
        }
    });
}

/// Enrolls node `name` and opens its session, saying it answers queries when it `announces`,
/// and answering as `how` says; its wire id.
async fn node(
    h: &Harness,
    (name, seed): (&str, u8),
    announces: bool,
    how: &Arc<Answering>,
) -> String {
    let agent = TestAgent::enroll(h, name, seed).await;
    let mut hello = agent.hello_at(SystemTime::now(), &[seed; 16]);
    if let Some(Up::Hello(said)) = hello.message.as_mut() {
        said.answers_feasibility = announces;
    }
    let (s, first) = TestAgent::open(h, hello).await;
    welcome(&first.unwrap());
    answer(s, Arc::clone(how));
    agent.node.clone()
}

/// Declares workload `name`; the placement it was given as it was declared.
async fn declared(h: &Harness, project: &str, repo: &str, name: &str) -> pb::Placement {
    let request = create(&format!("d-{name}"), project, name, body(repo, &[]));
    let out = h.workloads(h.root()).declare_intent(request).await.unwrap().into_owned();
    let get = pb::GetWorkloadRequest {
        id: out.workload.as_option().unwrap().id.clone(),
        ..Default::default()
    };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap().placement.into_option().expect("placed as declared")
}

/// The refusal of check `prepare` naming `node` in `p`'s explanation.
fn refusal<'a>(p: &'a pb::Placement, node: &str) -> &'a pb::FilterResult {
    let why = p.explanation.as_option().unwrap();
    why.filtered.iter().find(|r| r.check == "prepare" && r.node == node).expect("prepare refused")
}

/// How many refusals the control plane keeps.
async fn kept(h: &Harness) -> i64 {
    let store = h.db.store().await.unwrap();
    let count = sqlx::query_scalar("SELECT count(*) FROM placement_refusal");
    count.fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn a_node_whose_driver_refuses_is_passed_over_and_a_silent_one_for_that_bind_alone() {
    let h = Harness::new().await;
    // Scored alike, the first enrolled is preferred: its identifier is the lower.
    let first = Answering::new(false);
    let refusing = node(&h, ("fra-1", 1), true, &first).await;
    let second = Answering::new(true);
    let accepting = node(&h, ("fra-2", 2), true, &second).await;
    let project = project(&h, &h.root_org(), "games").await;
    let repo = push_paper().await;

    let placed = declared(&h, &project, &repo, "mc").await;
    assert_eq!(placed.node, accepting, "the refusing node is passed over");
    let refused = refusal(&placed, &refusing);
    assert!(refused.detail.contains("no such seccomp profile"), "{}", refused.detail);
    assert_eq!((first.asked(), second.asked()), (1, 1), "both shortlisted, asked once");
    assert_eq!(kept(&h).await, 1, "kept for the target");

    // The first node, now emptier and preferred, stops answering: passed over after five
    // seconds, and nothing is kept, so the next bind asks it again.
    first.answers.store(false, Ordering::SeqCst);
    let started = Instant::now();
    let placed = declared(&h, &project, &repo, "mc2").await;
    let waited = started.elapsed();
    assert_eq!(placed.node, accepting);
    assert!(refusal(&placed, &refusing).detail.contains("did not answer within 5 s"));
    assert!((Duration::from_secs(5)..Duration::from_secs(9)).contains(&waited), "{waited:?}");
    assert_eq!(kept(&h).await, 1, "a node that did not answer decided nothing");
    assert_eq!(first.asked(), 2);
}

#[tokio::test]
async fn an_agent_that_does_not_say_it_answers_is_not_asked_and_is_checked_after_the_bind() {
    let h = Harness::new().await;
    let would_refuse = Answering::new(false);
    let quiet = node(&h, ("fra-1", 1), false, &would_refuse).await;
    let project = project(&h, &h.root_org(), "games").await;
    let placed = declared(&h, &project, &push_paper().await, "mc").await;
    assert_eq!(placed.node, quiet, "bound unasked: an older agent is never waited on");
    assert_eq!(would_refuse.asked(), 0);
    assert_eq!(kept(&h).await, 0);
}
