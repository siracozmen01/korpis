// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shortlist query over this process's agent sessions (§2.1 of docs/05-scheduling.md): each
//! node whose agent said it answers is sent a `FeasibilityQuery` under a random identifier only it
//! is told, and its answer is taken from it alone. Every node is asked before any answer is awaited, so the wait is that of
//! the slowest node, bounded by the caller's limit.

use crate::reachability::{hex, random};
use crate::sessions::Sessions;
use korpis_app::ports::{Answers, Feasibility, Feasible};
use korpis_core::workload::spec::IntentBody;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Asks over the sessions this process holds.
#[derive(Debug)]
pub struct SessionFeasibility {
    sessions: Arc<Sessions>,
}

impl SessionFeasibility {
    /// The query over `sessions`.
    #[must_use]
    pub fn new(sessions: Arc<Sessions>) -> Self {
        Self { sessions }
    }

    /// Sends `node` the query; where its answer will arrive, or nothing when it cannot be sent.
    async fn send(
        &self,
        node: Uuid,
        body: &korpis_proto::korpis::v1::IntentBody,
        manifest: &[u8],
    ) -> Option<(String, tokio::sync::oneshot::Receiver<Feasible>)> {
        let sender = self.sessions.asker(node)?;
        let query = hex(&random::<16>()?);
        let answered = self.sessions.expect_feasibility(&query, node);
        let ask = agent::FeasibilityQuery {
            query: query.clone(),
            body: body.clone().into(),
            recipe_manifest: manifest.to_vec(),
            ..Default::default()
        };
        let message = agent::ControlMessage {
            message: Some(Down::FeasibilityQuery(Box::new(ask))),
            ..Default::default()
        };
        if sender.send(message).await.is_err() {
            self.sessions.forget_feasibility(&query);
            return None;
        }
        Some((query, answered))
    }

    /// `node`'s answer to `query`, awaited until `deadline`; unanswered after it.
    async fn answer(
        &self,
        query: &str,
        answered: tokio::sync::oneshot::Receiver<Feasible>,
        deadline: tokio::time::Instant,
    ) -> Feasible {
        if let Ok(Ok(answer)) = tokio::time::timeout_at(deadline, answered).await {
            return answer;
        }
        self.sessions.forget_feasibility(query);
        Feasible::Unanswered
    }

    /// Asks every node at once, then waits for their answers until `within` has passed.
    async fn ask_all(
        &self,
        nodes: &[Uuid],
        body: &IntentBody,
        manifest: &[u8],
        within: Duration,
    ) -> Answers {
        let deadline = tokio::time::Instant::now() + within;
        let body = korpis_wire::spec::body_to_pb(body);
        let mut asked = Vec::with_capacity(nodes.len());
        for node in nodes {
            let sent = self.send(*node, &body, manifest).await;
            asked.extend(sent.map(|(query, answered)| (*node, query, answered)));
        }
        let mut answers = Vec::with_capacity(asked.len());
        for (node, query, answered) in asked {
            answers.push((node, self.answer(&query, answered, deadline).await));
        }
        answers
    }
}

impl Feasibility for SessionFeasibility {
    fn ask<'a>(
        &'a self,
        nodes: &'a [Uuid],
        body: &'a IntentBody,
        manifest: &'a [u8],
        within: Duration,
    ) -> Pin<Box<dyn Future<Output = Answers> + Send + 'a>> {
        Box::pin(self.ask_all(nodes, body, manifest, within))
    }
}
