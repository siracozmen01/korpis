// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads, intents, power and plans, through the store ports, against the real schema.
//! Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod accounts;
mod buckets;
mod chain;
mod events;
mod ids;
mod jobs;
mod nodes;
mod placement;
mod plans;
mod revocation;
mod routes;
mod support;
mod trust;
mod volumes;
