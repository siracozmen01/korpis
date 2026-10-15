// SPDX-License-Identifier: AGPL-3.0-or-later

//! The session answering the control plane's feasibility queries (§2.1 of
//! docs/05-scheduling.md), each in a task of its own, at most [`QUERIES`] at once.

use super::Session;
use crate::feasibility::Answerer;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};

/// How many feasibility queries may be answered at once; the control plane asks a few nodes per
/// placement, so more waiting means the driver has stopped answering.
pub(super) const QUERIES: usize = 64;

impl Session {
    /// Answers feasibility queries with `answerer`, sending the answers on `outbound`, and says
    /// so in every `Hello`.
    #[must_use]
    pub fn answering(
        mut self,
        answerer: Answerer,
        outbound: mpsc::Sender<agent::AgentMessage>,
    ) -> Self {
        self.answering = Some((answerer, outbound, Arc::new(Semaphore::new(QUERIES))));
        self
    }

    /// Whether this session answers feasibility queries.
    pub(super) const fn answers(&self) -> bool {
        self.answering.is_some()
    }

    /// Answers `q` in a task of its own. A query the session cannot answer now, because it has
    /// no answerer or too many are being answered, is left unanswered: the control plane's
    /// timeout covers it.
    pub(super) fn answer(&self, q: Box<agent::FeasibilityQuery>) {
        let Some((answerer, outbound, room)) = &self.answering else { return };
        let Ok(permit) = Arc::clone(room).try_acquire_owned() else {
            tracing::warn!("a feasibility query was left unanswered: too many being answered");
            return;
        };
        let (asked, outbound) = (answerer(q), outbound.clone());
        tokio::spawn(async move {
            let result = asked.await;
            drop(permit);
            let up = Up::FeasibilityResult(Box::new(result));
            let _ = outbound
                .send(agent::AgentMessage { message: Some(up), ..Default::default() })
                .await;
        });
    }
}
