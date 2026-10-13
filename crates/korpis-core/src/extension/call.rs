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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallRequest {
    /// A liveness check every provider answers.
    Ping {
        /// Echoed back, so an answer is matched to its question.
        nonce: String,
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
    /// The provider, or the control plane on its behalf, failed the call.
    Failed(CallFailure),
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

    /// Nothing provides what the call names any more.
    #[must_use]
    pub fn unknown() -> Self {
        Self::of("provider.unknown", "no installation provides this here", false)
    }
}

/// One call, as the queue holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
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
