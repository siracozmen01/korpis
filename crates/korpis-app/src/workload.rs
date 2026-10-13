// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads: declaring what should be true, and the plans that get it there (§3.3 of
//! docs/01-model.md, §3 and §7 of docs/03-state.md).

mod admit;
mod cancel;
mod configure;
mod decide;
mod declare;
mod explain;
mod observed;
mod place;
mod power;
mod propose;
mod read;
mod retire;
mod watch;

pub use decide::{ApprovePlan, RejectPlan};
pub use declare::{DeclareIntent, DeclareTarget, Declared, NewWorkload};
pub use explain::{PlacementView, UnplacedView};
pub use observed::Observed;
pub use power::{ChangePower, PowerChanged};
pub use read::{ListWorkloads, WorkloadView};
pub use retire::DeleteWorkload;
pub use watch::Watch;
