// SPDX-License-Identifier: AGPL-3.0-or-later

//! A provider's condition as events (§5 of docs/15-observability.md): `provider.degraded` when
//! its breaker trips, carrying what was measured and what is queued behind it, and
//! `provider.recovered` when a probe closes the breaker again.

use super::ProviderType;
use super::breaker::{FailurePolicy, Measured, Trip};
use crate::event::Severity;
use std::time::SystemTime;
use uuid::Uuid;

/// What changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChange {
    /// Its breaker tripped.
    Degraded {
        /// Why.
        trip: Trip,
        /// What was measured.
        measured: Measured,
        /// How many calls were queued for it, waiting.
        queued: u64,
    },
    /// A probe closed its breaker.
    Recovered,
}

/// A provider's condition changed. Keyed to the provider, in the organization whose
/// installation serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCondition {
    /// The organization whose installation serves it.
    pub organization: Uuid,
    /// Its type.
    pub kind: ProviderType,
    /// Its name.
    pub name: String,
    /// When it was decided, on the control plane's clock.
    pub at: SystemTime,
    /// What changed.
    pub change: ProviderChange,
}

impl ProviderCondition {
    /// The event's kind.
    #[must_use]
    pub const fn event_kind(&self) -> &'static str {
        match self.change {
            ProviderChange::Degraded { .. } => "provider.degraded",
            ProviderChange::Recovered => "provider.recovered",
        }
    }

    /// The event's severity: degraded is a warning, and critical for a provider whose failure
    /// is failing closed, since then what it guards is refused to everyone.
    #[must_use]
    pub const fn severity(&self) -> Severity {
        match (self.change, self.kind.failure_policy()) {
            (ProviderChange::Recovered, _) => Severity::Info,
            (ProviderChange::Degraded { .. }, FailurePolicy::FailClosed) => Severity::Critical,
            (ProviderChange::Degraded { .. }, _) => Severity::Warning,
        }
    }

    /// The event's key: one ordered history per provider.
    #[must_use]
    pub fn key(&self) -> String {
        format!("provider:{}:{}:{}", self.organization, self.kind.as_str(), self.name)
    }
}
