// SPDX-License-Identifier: AGPL-3.0-or-later

//! The client API end to end: install into a throwaway database, serve over TLS, and call it as
//! a client would. Needs the development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod authority;
mod authz;
mod effects;
mod lifecycle;
mod nodes;
mod operator;
mod placement;
mod plans;
mod quota;
mod reads;
mod recipes;
mod retire;
mod support;
mod tenancy;
mod trust;
mod watch;
mod workload;
