// SPDX-License-Identifier: AGPL-3.0-or-later

//! Typed, permissioned configuration (§5 of docs/09-recipes.md, Rule K-17), run against the
//! example schema of that section.

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
