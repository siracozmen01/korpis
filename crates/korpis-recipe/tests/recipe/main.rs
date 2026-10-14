// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipes as OCI artifacts, end to end. The registry and resolve tests need the development
//! registry and database (`scripts/dev-registry.sh up`, `scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod artifact;
mod document;
mod extension;
mod installing;
mod registry;
mod resolve;
mod support;
