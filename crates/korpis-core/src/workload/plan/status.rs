// SPDX-License-Identifier: AGPL-3.0-or-later

//! A plan's status and the transitions between statuses (§3.3 of docs/01-model.md, §7 of
//! docs/03-state.md).

/// Where a plan is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    /// Computed, waiting for approval or confirmation.
    Proposed,
    /// Approved; the target has moved.
    Approved,
    /// Approved and waiting for its window or for the plan ahead of it.
    Queued,
    /// Being executed; at most one per workload.
    Applying,
    /// Every step completed.
    Applied,
    /// A step failed; resumable from `completed`.
    Failed,
    /// Stopped by a subject; resumable from `completed`.
    Cancelled,
    /// Refused by an approver.
    Rejected,
    /// Not approved before it expired.
    Expired,
    /// Computed against a state that has since moved; must be recomputed.
    Superseded,
}

impl PlanStatus {
    const ALL: [Self; 10] = [
        Self::Proposed,
        Self::Approved,
        Self::Queued,
        Self::Applying,
        Self::Applied,
        Self::Failed,
        Self::Cancelled,
        Self::Rejected,
        Self::Expired,
        Self::Superseded,
    ];

    /// The stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::Queued => "queued",
            Self::Applying => "applying",
            Self::Applied => "applied",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }

    /// The status a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Whether nothing further happens to the plan.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Applied
                | Self::Failed
                | Self::Cancelled
                | Self::Rejected
                | Self::Expired
                | Self::Superseded
        )
    }

    /// Whether the plan may move from this status to `to`. Only a proposed plan can be
    /// superseded, rejected or expire: once approved, the target has moved and the plan is
    /// executed or cancelled, never silently replaced.
    #[must_use]
    pub fn may_become(self, to: Self) -> bool {
        use PlanStatus as S;
        matches!(
            (self, to),
            (S::Proposed, S::Approved | S::Rejected | S::Expired | S::Superseded | S::Cancelled)
                | (S::Approved, S::Queued | S::Applying | S::Cancelled)
                | (S::Queued, S::Applying | S::Cancelled)
                | (S::Applying, S::Applied | S::Failed | S::Cancelled)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::PlanStatus as S;

    #[test]
    fn names_round_trip_and_terminal_states_go_nowhere() {
        for s in S::ALL {
            assert_eq!(S::parse(s.as_str()), Some(s));
            if s.is_terminal() {
                assert!(S::ALL.iter().all(|t| !s.may_become(*t)), "{s:?}");
            }
        }
        assert!(S::Proposed.may_become(S::Superseded));
        assert!(!S::Approved.may_become(S::Superseded), "an approved plan is not replaced");
        assert!(!S::Proposed.may_become(S::Applying), "nothing applies without approval");
    }
}
