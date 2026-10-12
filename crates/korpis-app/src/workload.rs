// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads: declaring what should be true, and the plans that get it there (§3.3 of
//! docs/01-model.md, §3 and §7 of docs/03-state.md).

mod admit;
mod configure;
mod declare;
mod propose;

pub use declare::{DeclareIntent, DeclareTarget, Declared, NewWorkload};
