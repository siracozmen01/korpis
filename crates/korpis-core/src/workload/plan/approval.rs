// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether a plan applies on its own, needs its proposer to confirm it, or needs someone else's
//! authority (§3.3 of docs/01-model.md, §3.6 of docs/08-identity.md).
//!
//! Two different things were both called approval, and the rules only hold once they are told
//! apart:
//!
//! - **Authority.** A plan proposed under a grant with `requires_approval_by` needs a different
//!   subject holding `plan.approve` over the approval scope; the proposer can never supply it.
//! - **Confirmation.** A plan its proposer was permitted to apply, but which is not
//!   auto-approvable (a disruptive or irreversible step, an unknown impact), waits for a human to
//!   confirm that they have read it. The proposer may confirm their own plan: that is
//!   `korpis apply --plan` (§4 of docs/13-surface-cli.md), and it is what keeps P5's promise that
//!   a human sees the change without making a lone operator unable to stop their own server.

use super::impact::Impact;
use super::step::Step;
use crate::scope::Scope;

/// How the plan was proposed: the decision the proposer's grant gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposing {
    /// The grant permits the action outright.
    Permitted,
    /// The grant only proposes; approval must come from within this scope.
    ProposeOnly {
        /// Where an approver must hold `plan.approve`.
        approval_scope: Scope,
    },
}

/// What a plan needs before it applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// Nothing: default policy approves it at once.
    Auto,
    /// Its proposer, or any approver, confirms it.
    Confirmation,
    /// A different subject with `plan.approve` over the scope approves it.
    Authority {
        /// Where the approver's grant must reach.
        scope: Scope,
    },
}

/// What `steps` with `impact`, proposed so, need; and why, for the plan's reader.
#[must_use]
pub fn required(steps: &[Step], impact: &Impact, proposing: &Proposing) -> (Approval, String) {
    if let Proposing::ProposeOnly { approval_scope } = proposing {
        let why = "the proposing grant requires approval".to_owned();
        return (Approval::Authority { scope: approval_scope.clone() }, why);
    }
    if let Some(s) = steps.iter().find(|s| !s.reversible) {
        return (Approval::Confirmation, format!("step {} cannot be undone", s.action.as_str()));
    }
    if let Some(s) = steps.iter().find(|s| s.disruptive) {
        return (
            Approval::Confirmation,
            format!("step {} is disruptive: {}", s.action.as_str(), s.reason),
        );
    }
    if !impact.downtime.is_known() || !impact.data_at_risk.is_known() {
        return (Approval::Confirmation, "its impact is unknown".into());
    }
    (Approval::Auto, "every step is non-disruptive and reversible".into())
}
