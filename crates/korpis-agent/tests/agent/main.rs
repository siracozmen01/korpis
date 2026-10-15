// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent against a real control plane: installed into a throwaway database and served on a
//! free port. Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod backup;
mod console;
mod console_write;
mod content;
mod data;
mod identity;
mod join;
mod journal;
mod lease;
mod offload;
mod reachability;
mod reconcile;
mod refused;
mod revocation;
mod session;
mod support;
