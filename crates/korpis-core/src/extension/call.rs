// SPDX-License-Identifier: AGPL-3.0-or-later

//! A provider call (§3.3 and §5 of docs/16-extensions.md): a request queued for one provider,
//! claimed by whichever control-plane replica holds a stream that serves it, sent down that
//! stream with a deadline, and finished by the provider's result or by the deadline.

use super::ProviderType;
use std::time::SystemTime;
use uuid::Uuid;

/// The longest a provider is waited on: no call is unbounded (§5 of docs/16-extensions.md).
pub const MAX_CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// What a call asks. Each provider type adds its own requests; a liveness check is common to all.
#[derive(Debug, Clone, PartialEq)]
pub enum CallRequest {
    /// A liveness check every provider answers.
    Ping {
        /// Echoed back, so an answer is matched to its question.
        nonce: String,
    },
    /// `dns`: make an owner's records exactly these (§3.5 of docs/07-networking.md).
    ApplyRecords {
        /// Who the records belong to.
        owner: String,
        /// Every record it should have.
        records: Vec<crate::dns::DnsRecord>,
    },
    /// `backup_target`: access to `prefix` for one job (§5.6 of docs/06-storage.md).
    GrantAccess {
        /// The repository prefix.
        prefix: String,
        /// What for.
        mode: super::AccessMode,
        /// How long.
        ttl: std::time::Duration,
    },
    /// `backup_target`: keys under `prefix` after `after`, at most `limit`.
    ListObjects {
        /// The repository prefix.
        prefix: String,
        /// The last key of the previous page; empty for the first.
        after: String,
        /// How many.
        limit: u32,
    },
    /// `backup_target`: delete these keys, with the target's own credential.
    DeleteObjects {
        /// The keys.
        keys: Vec<String>,
    },
    /// `step`: resolve an install step (§4 of docs/09-recipes.md).
    RunStep {
        /// The step kind, `<publisher>/<step>`.
        step: String,
        /// Its arguments, as the recipe gives them.
        arguments: crate::value::ValueMap,
        /// The workload being installed.
        workload: Uuid,
        /// The node's platform.
        platform: super::Platform,
    },
}

/// How a call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallOutcome {
    /// The answer to a [`CallRequest::Ping`].
    Pong {
        /// The nonce it was asked with.
        nonce: String,
    },
    /// The answer to [`CallRequest::GrantAccess`].
    Access(super::ObjectAccess),
    /// The answer to [`CallRequest::ListObjects`]: the objects, and whether more follow.
    Objects {
        /// The page.
        objects: Vec<super::StoredObject>,
        /// Whether more follow the last.
        more: bool,
    },
    /// The answer to [`CallRequest::DeleteObjects`].
    Deleted {
        /// How many were deleted.
        count: u32,
    },
    /// The answer to [`CallRequest::RunStep`].
    StepResult(super::StepResult),
    /// The answer to [`CallRequest::ApplyRecords`]: what it changed.
    RecordsApplied {
        /// Records created.
        created: u32,
        /// Records changed.
        updated: u32,
        /// Records removed.
        deleted: u32,
    },
    /// The provider, or the control plane on its behalf, failed the call.
    Failed(CallFailure),
}

impl CallRequest {
    /// Whether `outcome` answers this request: a failure answers anything, and a success only
    /// the request it is the answer to, in a form the control plane can act on. A provider
    /// answering otherwise is broken.
    #[must_use]
    pub fn answered_by(&self, outcome: &CallOutcome) -> bool {
        match (self, outcome) {
            (_, CallOutcome::Failed(_))
            | (Self::Ping { .. }, CallOutcome::Pong { .. })
            | (Self::ApplyRecords { .. }, CallOutcome::RecordsApplied { .. }) => true,
            (Self::RunStep { .. }, CallOutcome::StepResult(r)) => r.problem().is_none(),
            (Self::GrantAccess { prefix, .. }, CallOutcome::Access(a)) => {
                a.s3.key_prefix.ends_with(prefix.as_str())
            }
            (Self::ListObjects { prefix, limit, .. }, CallOutcome::Objects { objects, .. }) => {
                objects.len() <= usize::try_from(*limit).unwrap_or(usize::MAX)
                    && objects.iter().all(|o| super::is_beneath(&o.key, prefix))
            }
            (Self::DeleteObjects { keys }, CallOutcome::Deleted { count }) => {
                usize::try_from(*count).is_ok_and(|n| n <= keys.len())
            }
            _ => false,
        }
    }
}

/// A failed call, with a reason the plan or the observation shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFailure {
    /// A stable identifier, `provider.deadline` or the provider's own.
    pub reason: String,
    /// For display only.
    pub message: String,
    /// Whether asking again may succeed.
    pub retryable: bool,
}

impl CallFailure {
    /// Whether it says the provider is unwell, as its breaker counts failures: a retryable
    /// failure (its deadline, its stream, or the provider's own word), or an answer that answers
    /// nothing. A refusal of the request itself is a correct answer.
    #[must_use]
    pub fn unwell(&self) -> bool {
        self.retryable || self.reason == "provider.malformed"
    }

    /// A failure the control plane records on the provider's behalf.
    #[must_use]
    pub fn of(reason: &str, message: &str, retryable: bool) -> Self {
        Self { reason: reason.to_owned(), message: message.to_owned(), retryable }
    }

    /// No provider answered before the deadline.
    #[must_use]
    pub fn deadline() -> Self {
        Self::of("provider.deadline", "no provider answered before the deadline", true)
    }

    /// The stream the call was sent down ended before it was answered.
    #[must_use]
    pub fn stream_ended() -> Self {
        Self::of("provider.stream_ended", "the provider's stream ended before it answered", true)
    }

    /// The provider's breaker is open: it failed or was too slow lately, and is not called
    /// until it is probed again.
    #[must_use]
    pub fn circuit_open() -> Self {
        Self::of("provider.circuit_open", "the provider is failing; its breaker is open", true)
    }

    /// The provider answered with something that is not an answer to the call.
    #[must_use]
    pub fn malformed() -> Self {
        Self::of("provider.malformed", "the provider's answer does not answer the call", false)
    }

    /// Nothing provides what the call names any more.
    #[must_use]
    pub fn unknown() -> Self {
        Self::of("provider.unknown", "no installation provides this here", false)
    }
}

/// One call, as the queue holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCall {
    /// Its identifier.
    pub id: Uuid,
    /// The organization whose installation provides what it calls.
    pub organization: Uuid,
    /// The kind of provider called.
    pub kind: ProviderType,
    /// The `provides` entry called.
    pub name: String,
    /// What it asks.
    pub request: CallRequest,
    /// When the caller stops waiting.
    pub deadline: SystemTime,
    /// When it was queued.
    pub created_at: SystemTime,
    /// The registration it was sent to, and the provider version behind it, once claimed.
    pub sent: Option<(Uuid, String)>,
    /// How it ended, and when, once it has.
    pub finished: Option<(CallOutcome, SystemTime)>,
}
