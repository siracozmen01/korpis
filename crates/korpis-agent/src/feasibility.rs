// SPDX-License-Identifier: AGPL-3.0-or-later

//! Answering the control plane's `FeasibilityQuery` (§2.1 of docs/05-scheduling.md): the driver's
//! `Prepare` over the intent and the manifest the query carries. Nothing is fetched, created, or
//! kept; a query is answered whatever the node holds, since it is a question about a workload
//! the node does not run yet. The session answers, each query in a task of its own, and says in
//! its `Hello` that it does, so a control plane never waits on an agent that cannot.

use crate::runtime::{Prepared, Runtime};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// The workload name a queried intent is prepared under: the driver names what it would create
/// by it, and nothing is created.
const QUERIED: &str = "wl_feasibility";

/// What the driver says of `q`.
async fn answer_one<R: Runtime>(
    runtime: &R,
    q: &agent::FeasibilityQuery,
) -> agent::FeasibilityResult {
    let intent = pb::Intent { body: q.body.clone(), ..Default::default() };
    let mut result = agent::FeasibilityResult { query: q.query.clone(), ..Default::default() };
    match runtime.prepare(QUERIED, &intent, &q.recipe_manifest).await {
        Ok(Prepared::Spec(_)) => result.accepted = true,
        Ok(Prepared::Rejected(r)) => {
            result.reason = r.reason;
            result.detail = r.detail;
        }
        Err(e) => {
            result.unanswered = true;
            result.detail = e.to_string();
        }
    }
    result
}

/// Answers one query: what the session calls, so the session needs no runtime of its own.
pub type Answerer = Arc<
    dyn Fn(
            Box<agent::FeasibilityQuery>,
        ) -> Pin<Box<dyn Future<Output = agent::FeasibilityResult> + Send>>
        + Send
        + Sync,
>;

/// The answerer over `runtime`.
pub fn answerer<R: Runtime + Clone>(runtime: R) -> Answerer {
    Arc::new(move |q| {
        let runtime = runtime.clone();
        Box::pin(async move { answer_one(&runtime, &q).await })
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use crate::runtime::memory::InMemory;
    use korpis_proto::korpis::driver::v1 as driver;

    fn query() -> agent::FeasibilityQuery {
        agent::FeasibilityQuery { query: "q1".into(), ..Default::default() }
    }

    #[tokio::test]
    async fn a_query_is_answered_with_what_prepare_says_and_creates_nothing() {
        let runtime = InMemory::new();
        let accepted = answer_one(&runtime, &query()).await;
        assert!(accepted.accepted && !accepted.unanswered);
        assert_eq!(accepted.query, "q1");
        let refusal = driver::Rejection {
            reason: pb::UnsatisfiableReason::UNSATISFIABLE_REASON_PREPARE_REJECTED.into(),
            detail: "no such seccomp profile".into(),
            ..Default::default()
        };
        runtime.reject(Some(refusal));
        let refused = answer_one(&runtime, &query()).await;
        assert!(!refused.accepted && !refused.unanswered);
        assert_eq!(refused.detail, "no such seccomp profile");
        runtime.reject(None);
        runtime.fail("prepare", true);
        let failed = answer_one(&runtime, &query()).await;
        assert!(!failed.accepted && failed.unanswered, "a driver that fails decides nothing");
        assert!(runtime.objects().is_empty());
    }
}
