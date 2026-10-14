// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads: declaring what should be true, and the plans that get it there (§3.3 of
//! docs/01-model.md, §3 and §7 of docs/03-state.md).

mod admit;
mod attach;
mod cancel;
mod classify;
mod conditions;
mod configure;
mod decide;
mod declare;
mod explain;
mod observed;
mod pin;
mod place;
mod power;
mod propose;
mod read;
mod retire;
mod sets;
mod stream;
mod volume_plan;
mod watch;

pub use classify::{Classified, SetClassification};
pub use decide::{ApprovePlan, RejectPlan};
pub use declare::{DeclareIntent, DeclareTarget, Declared, NewWorkload, SetDeclaration};
pub use explain::{PlacementView, UnplacedView};
pub use observed::Observed;
pub use pin::PlaceWorkload;
pub use power::{ChangePower, PowerChanged};
pub use read::{Drift, ListWorkloads, WorkloadView};
pub use retire::DeleteWorkload;
pub use sets::SetDriftMode;
pub use stream::StreamView;
pub use watch::Watch;
