// SPDX-License-Identifier: AGPL-3.0-or-later

//! An operation: one decision over many items, approved once, generating each item's plan as it
//! proceeds and never ahead of it, since a plan computed an hour earlier is superseded on arrival.
//! Draining a node is one decision about every workload on it, and approving each workload's plan
//! by hand is not a workflow anyone uses. What an operation may do is its approval's: the approver's
//! grant, read again at every step it takes.

use crate::principal::Principal;
use std::time::SystemTime;
use uuid::Uuid;

pub mod item;

pub use item::{Admission, Item, ItemState, Progress};

/// The most items an operation applies at once: past this, a node's network, not the policy, is
/// what limits a drain, and an operator who asks for more has asked for a self-inflicted outage.
pub const MOST_CONCURRENT: u32 = 64;

/// What an operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Moves every workload off a node that takes part, by migration.
    Drain,
    /// Places every workload of a node that cannot take part again elsewhere, from its intent.
    Evacuate,
    /// Gives every grant issued from a template what a later version of it adds.
    TemplateRollout,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Self; 3] = [Self::Drain, Self::Evacuate, Self::TemplateRollout];

    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Drain => "drain",
            Self::Evacuate => "evacuate",
            Self::TemplateRollout => "template_rollout",
        }
    }

    /// The kind named `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// What an operation is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every workload placed on a node: a drain's and an evacuation's.
    Node(Uuid),
    /// Every live grant issued from a template at an earlier version: a rollout's of `version`.
    Template {
        /// The template.
        template: Uuid,
        /// The version whose additions are given.
        version: u64,
    },
}

impl Scope {
    /// Whether an operation of `kind` may be over it.
    #[must_use]
    pub const fn fits(self, kind: Kind) -> bool {
        matches!(
            (kind, self),
            (Kind::Drain | Kind::Evacuate, Self::Node(_))
                | (Kind::TemplateRollout, Self::Template { .. })
        )
    }
}

/// What an operation does when one of its items fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnFailure {
    /// Stops beginning items until a person resumes it.
    Pause,
    /// Goes on with the rest; the failure is counted and named.
    Continue,
    /// Fails the operation: it begins nothing more.
    Abort,
}

impl OnFailure {
    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Continue => "continue",
            Self::Abort => "abort",
        }
    }

    /// The policy named `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Pause, Self::Continue, Self::Abort].into_iter().find(|o| o.as_str() == s)
    }
}

/// How an operation proceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// The most items applying at once.
    pub concurrency: u32,
    /// The most of its workloads it leaves unavailable at once: those applying, and those it moved
    /// that are not yet seen running where they now are.
    pub max_unavailable: u32,
    /// What a failed item does to the rest.
    pub on_failure: OnFailure,
}

impl Default for Policy {
    /// One at a time, pausing at the first failure: the slowest drain, and the one that never
    /// makes an outage worse than one workload.
    fn default() -> Self {
        Self { concurrency: 1, max_unavailable: 1, on_failure: OnFailure::Pause }
    }
}

/// Why a policy is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// Concurrency is zero or past [`MOST_CONCURRENT`].
    #[error("concurrency is from 1 to {MOST_CONCURRENT}")]
    Concurrency,
    /// `max_unavailable` is zero, which would begin nothing.
    #[error("max_unavailable is at least 1")]
    MaxUnavailable,
}

impl PolicyError {
    /// The field it is about.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::Concurrency => "policy.concurrency",
            Self::MaxUnavailable => "policy.max_unavailable",
        }
    }
}

impl Policy {
    /// Checks it.
    pub const fn validate(&self) -> Result<(), PolicyError> {
        if self.concurrency == 0 || self.concurrency > MOST_CONCURRENT {
            return Err(PolicyError::Concurrency);
        }
        if self.max_unavailable == 0 {
            return Err(PolicyError::MaxUnavailable);
        }
        Ok(())
    }
}

/// Where an operation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting for a person's approval.
    Proposed,
    /// Approved, waiting for the time it was approved to start at.
    Approved,
    /// Beginning items as its policy allows.
    Running,
    /// Beginning nothing until a person resumes it; what applies goes on.
    Paused,
    /// Every item done.
    Completed,
    /// Every item ended and some failed, or one failed under `abort`.
    Failed,
    /// Stopped by a person; what applied goes on, and nothing more begins.
    Aborted,
}

impl Status {
    /// Every status.
    pub const ALL: [Self; 7] = [
        Self::Proposed,
        Self::Approved,
        Self::Running,
        Self::Paused,
        Self::Completed,
        Self::Failed,
        Self::Aborted,
    ];

    /// Its name on the wire and in the store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }

    /// The status named `s`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Whether it has ended, for good.
    #[must_use]
    pub const fn ended(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Aborted)
    }
}

/// A change of status the operation's status does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an operation that is {from} cannot be {to}")]
pub struct TransitionError {
    /// Its status.
    pub from: &'static str,
    /// What was asked.
    pub to: &'static str,
}

/// Who approved an operation, under what, and from when it may begin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approved {
    /// The person.
    pub by: Principal,
    /// The grant they approved under, which every item is carried out under.
    pub under: Uuid,
    /// Whether their credential carried a second factor as they approved.
    pub mfa: bool,
    /// When.
    pub at: SystemTime,
    /// The earliest it begins; `None` for at once.
    pub start_after: Option<SystemTime>,
}

/// An operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// Its identifier.
    pub id: Uuid,
    /// What it does.
    pub kind: Kind,
    /// What it is over.
    pub scope: Scope,
    /// How it proceeds.
    pub policy: Policy,
    /// Where it is.
    pub status: Status,
    /// Why it paused, failed, or was aborted; empty otherwise.
    pub reason: String,
    /// Who proposed it.
    pub proposed_by: Principal,
    /// The grant it was proposed under.
    pub proposed_under: Uuid,
    /// When.
    pub proposed_at: SystemTime,
    /// Its approval, once given.
    pub approved: Option<Approved>,
    /// When it ended.
    pub ended_at: Option<SystemTime>,
    /// Its version, raised by every change.
    pub version: u64,
}

impl Operation {
    fn refuse(&self, to: &'static str) -> TransitionError {
        TransitionError { from: self.status.as_str(), to }
    }

    /// Approves it, as `approved` says.
    pub fn approve(&mut self, approved: Approved) -> Result<(), TransitionError> {
        if self.status != Status::Proposed {
            return Err(self.refuse("approved"));
        }
        self.status = Status::Approved;
        self.approved = Some(approved);
        Ok(())
    }

    /// Starts it, if it is approved and its start has come; whether it did.
    pub fn start(&mut self, now: SystemTime) -> bool {
        let due = self.approved.as_ref().is_some_and(|a| a.start_after.is_none_or(|s| s <= now));
        if self.status != Status::Approved || !due {
            return false;
        }
        self.status = Status::Running;
        true
    }

    /// Pauses it, for `why`.
    pub fn pause(&mut self, why: &str) -> Result<(), TransitionError> {
        if !matches!(self.status, Status::Approved | Status::Running) {
            return Err(self.refuse("paused"));
        }
        self.status = Status::Paused;
        why.clone_into(&mut self.reason);
        Ok(())
    }

    /// Resumes it.
    pub fn resume(&mut self) -> Result<(), TransitionError> {
        if self.status != Status::Paused {
            return Err(self.refuse("resumed"));
        }
        self.status = Status::Running;
        self.reason.clear();
        Ok(())
    }

    /// Aborts it, at `now`, for `why`.
    pub fn abort(&mut self, why: &str, now: SystemTime) -> Result<(), TransitionError> {
        if self.status.ended() {
            return Err(self.refuse("aborted"));
        }
        self.status = Status::Aborted;
        why.clone_into(&mut self.reason);
        self.ended_at = Some(now);
        Ok(())
    }

    /// What one of its items failing, for `why`, does to it at `now`, as its policy says.
    pub fn item_failed(&mut self, why: &str, now: SystemTime) {
        if self.status != Status::Running {
            return;
        }
        match self.policy.on_failure {
            OnFailure::Pause => {
                self.status = Status::Paused;
                why.clone_into(&mut self.reason);
            }
            OnFailure::Continue => {}
            OnFailure::Abort => {
                self.status = Status::Failed;
                why.clone_into(&mut self.reason);
                self.ended_at = Some(now);
            }
        }
    }

    /// Ends it at `now` once every item has: completed when none failed. Whether it ended.
    pub fn conclude(&mut self, progress: &Progress, now: SystemTime) -> bool {
        if self.status != Status::Running || !progress.ended() {
            return false;
        }
        if progress.failed == 0 {
            self.status = Status::Completed;
        } else {
            self.status = Status::Failed;
            self.reason = format!("{} of {} items failed", progress.failed, progress.total);
        }
        self.ended_at = Some(now);
        true
    }
}

#[cfg(test)]
mod tests;
