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

mod agent;
mod assignments;
mod authority;
mod authz;
mod backup_target;
mod cancel;
mod classify;
mod console;
mod denials;
mod dns;
mod effects;
mod enroll;
mod events;
mod extensions;
mod identity;
mod journal;
mod leases;
mod lifecycle;
mod nodes;
mod observe;
mod operator;
mod pin;
mod placement;
mod plans;
mod provider_breaker;
mod provider_calls;
mod providers;
mod proxy;
mod quota;
mod reads;
mod recipes;
mod relay;
mod relays;
mod renewal;
mod retire;
mod revocation;
mod sdk;
mod session;
mod sets;
mod standalone;
mod step;
mod streams;
mod support;
mod tenancy;
mod tokens;
mod trust;
mod upgrade;
mod volumes;
mod watch;
mod workload;
