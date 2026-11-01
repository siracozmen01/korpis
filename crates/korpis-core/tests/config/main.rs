// SPDX-License-Identifier: AGPL-3.0-or-later

//! Typed, permissioned configuration, run against the example schema of that section.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod deliver;
mod resolve;
mod schema;
mod support;
