// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workload and its state (§3.3 of docs/01-model.md): its identity, the intents declared
//! for it, its power declaration, and the plans that move it from one to the next.

pub mod assignment;
pub mod declare;
pub mod plan;
pub mod spec;
pub mod status;

pub use plan::{Plan, PlanStatus, PlanTarget};
pub use spec::{IntentBody, SpecError};

use crate::labels::Labels;
use crate::principal::Principal;
use crate::tenancy::Name;
use std::time::SystemTime;
use uuid::Uuid;

/// The identity of a thing that runs: a stable name to which state attaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workload {
    /// Immutable, never reused, even after deletion.
    pub id: Uuid,
    /// The project it belongs to.
    pub project: Uuid,
    /// The organization that owns the project, for authorization.
    pub organization: Uuid,
    /// For humans; unique within the project among workloads not deleted.
    pub name: Name,
    /// For selectors.
    pub labels: Labels,
    /// The head: the newest declared intent.
    pub intent: Uuid,
    /// The head's version.
    pub intent_version: u64,
    /// The approved intent nodes converge toward; `None` until the first plan is approved.
    pub target: Option<Uuid>,
    /// Bumped every time the target moves; approval is a compare-and-swap on it (§3.2 of
    /// docs/03-state.md).
    pub target_version: u64,
    /// The newest power declaration's version: the power head.
    pub power_version: u64,
    /// The approved power declaration nodes are told, by version; zero until one is approved.
    /// Moved only by approving a plan, like `target` (§3.3 of docs/01-model.md).
    pub power_target_version: u64,
    /// Bumped on every metadata change, for compare-and-swap.
    pub version: u64,
    /// When it was created.
    pub created: SystemTime,
    /// Soft deletion; the identifier is retained forever.
    pub deleted_at: Option<SystemTime>,
}

/// What someone declared should be true. Immutable: a change is a new intent with the next
/// version and the previous one as parent.
#[derive(Debug, Clone, PartialEq)]
pub struct Intent {
    /// Immutable identity.
    pub id: Uuid,
    /// The workload it declares.
    pub workload: Uuid,
    /// Monotonic per workload, from 1.
    pub version: u64,
    /// The intent it was derived from.
    pub parent: Option<Uuid>,
    /// Who declared it; there is no anonymous change.
    pub declared_by: Principal,
    /// When.
    pub declared_at: SystemTime,
    /// The declaration.
    pub body: IntentBody,
}

/// Whether the workload should be running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    /// Running.
    Running,
    /// Stopped.
    Stopped,
}

/// The power declaration: versioned and attributed like an intent, and kept out of the intent
/// so that the intent chain stays a configuration history (finding R7, §3.3 of
/// docs/01-model.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Power {
    /// Running or stopped.
    pub desired: PowerState,
    /// Incremented to request one restart; performed once per value.
    pub cycle: u64,
    /// Monotonic, its own sequence, from 1.
    pub version: u64,
}

impl Power {
    /// The first declaration of a new workload.
    #[must_use]
    pub fn initial(desired: PowerState) -> Self {
        Self { desired, cycle: 0, version: 1 }
    }

    /// The declaration that follows this one for a start, a stop, or a restart.
    #[must_use]
    pub fn next(self, change: PowerChange) -> Self {
        let version = self.version + 1;
        match change {
            PowerChange::Start => Self { desired: PowerState::Running, version, ..self },
            PowerChange::Stop => Self { desired: PowerState::Stopped, version, ..self },
            PowerChange::Restart => {
                Self { desired: PowerState::Running, cycle: self.cycle + 1, version }
            }
        }
    }
}

/// A power operation a subject asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerChange {
    /// `workload.start`.
    Start,
    /// `workload.stop`.
    Stop,
    /// `workload.restart`: a higher cycle, running.
    Restart,
}

/// A power declaration with its attribution, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PowerRecord {
    /// The workload.
    pub workload: Uuid,
    /// The declaration.
    pub power: Power,
    /// Who set it.
    pub set_by: Principal,
    /// When.
    pub set_at: SystemTime,
}
