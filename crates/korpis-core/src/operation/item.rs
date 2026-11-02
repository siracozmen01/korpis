// SPDX-License-Identifier: AGPL-3.0-or-later

//! An operation's items, one per workload or grant it is over, fixed when it is approved, and how
//! many may begin now. An item applies while its plan does; one that moved a workload then settles
//! until the workload is seen running where it now is, and counts as unavailable until then.

use super::Policy;
use std::time::SystemTime;
use uuid::Uuid;

/// Where an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    /// Not begun.
    Waiting,
    /// Its plan applies.
    Applying,
    /// Its workload moved, and is not yet seen running where it now is.
    Settling,
    /// Done.
    Done,
    /// Failed; the reason says why.
    Failed,
    /// Left alone, since it no longer needed doing: a workload deleted, or moved off the node
    /// meanwhile. A grant revoked before its turn is not skipped: it fails, alone.
    Skipped,
}

impl ItemState {
    /// Every state.
    pub const ALL: [Self; 6] =
        [Self::Waiting, Self::Applying, Self::Settling, Self::Done, Self::Failed, Self::Skipped];

    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Applying => "applying",
            Self::Settling => "settling",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// The state named `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Whether it has ended.
    #[must_use]
    pub const fn ended(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Skipped)
    }
}

/// One item of an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The operation.
    pub operation: Uuid,
    /// What it is about: a workload, or a grant.
    pub subject: Uuid,
    /// Its place in the order items begin in.
    pub position: u32,
    /// Where it is.
    pub state: ItemState,
    /// The plan generated for it, for a workload.
    pub plan: Option<Uuid>,
    /// The grant issued for it, for a grant.
    pub issued: Option<Uuid>,
    /// What it was found to need when the operation was proposed, or did when it applied: where a
    /// workload's data comes from and what is at risk, or the actions a grant is given.
    pub detail: serde_json::Map<String, serde_json::Value>,
    /// Why it failed or was skipped; empty otherwise.
    pub reason: String,
    /// When it began.
    pub begun_at: Option<SystemTime>,
    /// When it ended.
    pub ended_at: Option<SystemTime>,
}

impl Item {
    /// A waiting item about `subject`, `position`th of `operation`'s.
    #[must_use]
    pub fn waiting(operation: Uuid, subject: Uuid, position: u32) -> Self {
        Self {
            operation,
            subject,
            position,
            state: ItemState::Waiting,
            plan: None,
            issued: None,
            detail: serde_json::Map::new(),
            reason: String::new(),
            begun_at: None,
            ended_at: None,
        }
    }

    /// Ends it in `state`, for `why`, at `now`.
    pub fn end(&mut self, state: ItemState, why: &str, now: SystemTime) {
        self.state = state;
        why.clone_into(&mut self.reason);
        self.ended_at = Some(now);
    }
}

/// How far an operation is, by its items.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Every item.
    pub total: u32,
    /// Not begun.
    pub waiting: u32,
    /// Applying.
    pub applying: u32,
    /// Settling.
    pub settling: u32,
    /// Done.
    pub done: u32,
    /// Failed.
    pub failed: u32,
    /// Skipped.
    pub skipped: u32,
}

impl Progress {
    /// The progress of `items`.
    #[must_use]
    pub fn of<'a>(items: impl IntoIterator<Item = &'a Item>) -> Self {
        let mut p = Self::default();
        for i in items {
            p.total += 1;
            let count = match i.state {
                ItemState::Waiting => &mut p.waiting,
                ItemState::Applying => &mut p.applying,
                ItemState::Settling => &mut p.settling,
                ItemState::Done => &mut p.done,
                ItemState::Failed => &mut p.failed,
                ItemState::Skipped => &mut p.skipped,
            };
            *count += 1;
        }
        p
    }

    /// Whether every item has ended.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.waiting == 0 && self.applying == 0 && self.settling == 0
    }
}

/// How many more items may begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission(pub u32);

impl Admission {
    /// How many items may begin under `policy` with `progress` as it is: no more than leave
    /// `concurrency` applying, and no more than leave `max_unavailable` applying or settling,
    /// since any applying item may stop its workload, and none more than are waiting.
    #[must_use]
    pub fn under(policy: &Policy, progress: &Progress) -> Self {
        let by_concurrency = policy.concurrency.saturating_sub(progress.applying);
        let unavailable = progress.applying + progress.settling;
        let by_unavailable = policy.max_unavailable.saturating_sub(unavailable);
        Self(by_concurrency.min(by_unavailable).min(progress.waiting))
    }
}
