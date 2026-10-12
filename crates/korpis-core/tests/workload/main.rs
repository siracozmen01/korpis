// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workload model of §3.3 of docs/01-model.md: declarations, plans, and approval.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod approval;
mod declare;
mod plans;
mod quota;
mod spec;
mod support;
