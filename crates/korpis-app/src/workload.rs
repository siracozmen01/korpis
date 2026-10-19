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
mod dependency;
mod explain;
mod hold;
mod learned;
mod observed;
mod pin;
mod place;
mod placed_quota;
pub(crate) use place::EDGE_TIER;
mod power;
mod propose;
mod prune_plan;
mod reached;
mod read;
mod recipe;
mod replace;
mod retire;
mod route;
mod screen;
mod seed;
mod sets;
mod stream;
pub(crate) mod volume_plan;
mod watch;
mod withhold;

pub use classify::{Classified, SetClassification};
pub use decide::{ApprovePlan, RejectPlan};
pub use declare::{DeclareIntent, DeclareTarget, Declared, NewWorkload, SetDeclaration};
pub use explain::{PlacementView, UnplacedView};
pub use observed::Observed;
pub use pin::PlaceWorkload;
pub use power::{ChangePower, PowerChanged};
pub use read::{Drift, ListWorkloads, Withheld, WorkloadView};
pub use recipe::{MAX_LOCALES, WorkloadRecipe};
pub(crate) use replace::refused_before_anything;
pub use retire::DeleteWorkload;
pub use sets::SetDriftMode;
pub use stream::StreamView;
pub use watch::Watch;
pub use withhold::Shown;
