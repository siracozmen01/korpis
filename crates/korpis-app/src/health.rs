// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's query probe, the control plane's half (§5 of docs/04-runtimes.md, §5 of
//! docs/16-extensions.md). The node runs the exchange from inside the workload's network
//! namespace and asks, after each round, what comes next; the control plane checks that the node
//! is where the workload is placed and that the probe is a query probe of the intent the node
//! names, calls the health provider that intent pinned with the rounds so far, and answers with
//! the provider's next round or verdict. Anything that keeps the probe from being judged, the
//! provider's failure or its breaker among them, is answered as unjudged: a workload's health is
//! then unknown, never unhealthy, so a failed prober cannot get what it probes restarted.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::extension::health::{HealthNext, HealthStep, MAX_ROUNDS, Round, Transport};
use korpis_core::extension::{CallOutcome, CallRequest, ProviderType};
use korpis_core::workload::spec::endpoints::Protocol;
use korpis_core::workload::spec::health::ProbeKind;
use std::time::Duration;
use uuid::Uuid;

/// The longest a provider is given for one step, whatever the node would wait.
pub const MOST_WITHIN: Duration = Duration::from_secs(10);

/// Which of a workload's probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeRole {
    /// When it can take traffic.
    Readiness,
    /// When it should be restarted.
    Liveness,
}

/// A node's question: what comes after `rounds` in the exchange of `probe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthQuery {
    /// The workload probed.
    pub workload: Uuid,
    /// The intent its runtime object was built from.
    pub intent: Uuid,
    /// Which probe.
    pub probe: ProbeRole,
    /// The rounds run so far.
    pub rounds: Vec<Round>,
    /// How long the node waits for the answer.
    pub within: Duration,
}

/// Why a step of a query probe was not answered with a round or a verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unjudged {
    /// The workload is not placed on the node, or the intent is not the workload's.
    NotAssigned,
    /// The probe named is not a query probe of the intent, or the question is malformed.
    NotAQuery,
    /// The installation the intent pinned no longer answers for the protocol.
    ProviderChanged,
    /// The provider's own failure, or its breaker's.
    Failed {
        /// Its stable reason.
        reason: String,
        /// Whether asking again may succeed.
        retryable: bool,
    },
}

impl Unjudged {
    /// The stable reason the node is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NotAssigned => "health.not_assigned".into(),
            Self::NotAQuery => "health.not_a_query".into(),
            Self::ProviderChanged => "health.provider_changed".into(),
            Self::Failed { reason, .. } => reason.clone(),
        }
    }

    /// Whether asking again, unchanged, may succeed.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Failed { retryable, .. } => *retryable,
            _ => false,
        }
    }
}

type Answered = Result<Result<HealthNext, Unjudged>, AppError>;

/// How an endpoint of `protocol` is reached for a query.
const fn transport(protocol: Protocol) -> Transport {
    match protocol {
        Protocol::Udp | Protocol::Quic => Transport::Udp,
        Protocol::Tcp | Protocol::Http | Protocol::Https => Transport::Tcp,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Answers `node`'s question `q`: the health provider's next round or verdict.
    pub async fn health_step(&self, node: Uuid, q: HealthQuery) -> Answered {
        if self.store.placement(q.workload).await?.is_none_or(|p| p.node != node) {
            return Ok(Err(Unjudged::NotAssigned));
        }
        let Some(intent) = self.store.intent(q.intent).await?.filter(|i| i.workload == q.workload)
        else {
            return Ok(Err(Unjudged::NotAssigned));
        };
        let health = &intent.body.health;
        let probe = match q.probe {
            ProbeRole::Readiness => health.readiness.as_ref(),
            ProbeRole::Liveness => health.liveness.as_ref(),
        };
        let Some(ProbeKind::Query { protocol, endpoint }) = probe.map(|p| &p.kind) else {
            return Ok(Err(Unjudged::NotAQuery));
        };
        let reached = intent.body.endpoints.iter().find(|e| &e.name == endpoint);
        let (Some(reached), true) = (reached, q.rounds.len() <= MAX_ROUNDS) else {
            return Ok(Err(Unjudged::NotAQuery));
        };
        let workload = self.store.workload(q.workload).await?;
        let workload = workload.ok_or(AppError::NotFound { kind: "workload" })?;
        let target = CallTarget {
            organization: workload.organization,
            kind: ProviderType::Health,
            name: protocol.clone(),
        };
        let providers = &intent.body.recipe.providers;
        let pin = providers.iter().find(|p| p.kind == ProviderType::Health && &p.name == protocol);
        if !self.still_answers(&target, pin).await? {
            return Ok(Err(Unjudged::ProviderChanged));
        }
        let step = HealthStep {
            protocol: protocol.clone(),
            workload: q.workload,
            endpoint: endpoint.clone(),
            transport: transport(reached.protocol),
            rounds: q.rounds,
        };
        let within = q.within.min(MOST_WITHIN);
        let called = self.call_provider(&target, CallRequest::HealthStep(step), within).await?;
        Ok(match called.outcome {
            CallOutcome::HealthNext(next) => Ok(next),
            CallOutcome::Failed(f) => {
                Err(Unjudged::Failed { reason: f.reason, retryable: f.retryable })
            }
            _ => Err(Unjudged::Failed { reason: "provider.malformed".into(), retryable: true }),
        })
    }
}
