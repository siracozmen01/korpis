// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a use case can answer instead of a result. Each variant is a decision a client can act
//! on (§8 of docs/10-api.md); the driving adapter chooses how to say it on its transport.

use crate::ports::{StoreError, Unique};

/// A use case's refusal or failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    /// The request is malformed; `field` names what to fix.
    #[error("{field}: {message}")]
    Invalid {
        /// The offending field, in the request's own terms.
        field: String,
        /// What is wrong with it.
        message: String,
    },
    /// No valid credential.
    #[error("a valid credential is required")]
    Unauthenticated,
    /// Refused, and the caller may know the object exists.
    #[error("missing permission {action}")]
    Denied {
        /// The action no grant carried.
        action: String,
    },
    /// A person acting through a relay asked for something other than the one action their
    /// ticket names, or for something no ticket can name (§5.2 of docs/08-identity.md).
    #[error("a relayed call performs only the one action its ticket names")]
    Relayed,
    /// The caller may make the change but not set this field: an operator field of a recipe's
    /// configuration needs authority over the owning organization's parent (§5 of
    /// docs/09-recipes.md).
    #[error("{field} needs {action} over the owning organization's parent")]
    FieldDenied {
        /// The field, in the request's own terms.
        field: String,
        /// The action no grant carried there.
        action: String,
    },
    /// Does not exist, or the caller may not know that it does.
    #[error("{kind} not found")]
    NotFound {
        /// What kind of object.
        kind: &'static str,
    },
    /// The request conflicts with current state; retrying unchanged will not help unless
    /// `reason` says otherwise.
    #[error("{message}")]
    Conflict {
        /// A stable reason clients branch on, e.g. `organization.name_taken`.
        reason: &'static str,
        /// For humans.
        message: String,
    },
    /// Admitting the change would exceed a quota entry (§5.1 of docs/05-scheduling.md).
    #[error("{message}")]
    QuotaExceeded {
        /// The dimension, as `korpis.v1.Resources` names it.
        dimension: &'static str,
        /// For humans: which entry, its bound, and what would be held.
        message: String,
    },
    /// The organization's trust policy does not let it use this recipe (§7 of
    /// docs/09-recipes.md). Not a permission of the caller's: no grant changes it, only the
    /// policy or a signature.
    #[error("{message}")]
    Untrusted {
        /// For humans: which digest, and that no accepted signer signed it.
        message: String,
    },
    /// No state of the system can satisfy the request as asked: a node that cannot run the
    /// workload, whoever chooses it (§8 of docs/10-api.md).
    #[error("{message}")]
    Unsatisfiable {
        /// A stable reason clients branch on, e.g. `placement.unsatisfiable`.
        reason: &'static str,
        /// For humans: every check that failed.
        message: String,
    },
    /// Specified, scheduled, not yet built (§4 of docs/20-roadmap.md).
    #[error("{what} arrive in Phase {phase}")]
    NotYet {
        /// What is missing.
        what: &'static str,
        /// The roadmap phase that builds it.
        phase: u8,
    },
    /// A rate limit bucket refused the request (§10 of docs/10-api.md); it charged none.
    #[error("rate limited: retry after {retry_after:?}")]
    RateLimited {
        /// When the fullest refusing bucket admits it.
        retry_after: std::time::Duration,
        /// Where each bucket stands.
        standing: Vec<korpis_core::rate::Standing>,
    },
    /// Something Korpis depends on could not be reached; retrying may help.
    #[error("{what} is unavailable")]
    Unavailable {
        /// What could not be reached.
        what: &'static str,
        /// Why, for the operator's log.
        detail: String,
    },
    /// Our fault. The message is for the operator's log, not for the caller.
    #[error("internal: {0}")]
    Internal(String),
}

impl AppError {
    /// A malformed field.
    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Invalid { field: field.into(), message: message.into() }
    }

    /// A conflict with a stable reason.
    pub fn conflict(reason: &'static str, message: impl Into<String>) -> Self {
        Self::Conflict { reason, message: message.into() }
    }
}

impl From<StoreError> for AppError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Duplicate(Unique::IdempotencyKey) => Self::conflict(
                "idempotency.in_flight",
                "a request with this idempotency key is in flight; retry it",
            ),
            StoreError::Contended(_) => Self::conflict(
                "store.contended",
                "another change contended with this one and it was undone; retry it",
            ),
            other => Self::Internal(other.to_string()),
        }
    }
}
