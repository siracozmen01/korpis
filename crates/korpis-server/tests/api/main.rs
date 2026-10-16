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

mod acme;
mod agent;
mod assignments;
mod authority;
mod authz;
mod backup_read;
mod backup_target;
mod backups;
mod cancel;
mod certificates;
mod churn;
mod classify;
mod console;
mod denials;
mod dependencies;
mod dns;
mod domains;
mod effects;
mod egress;
mod endpoints;
mod enroll;
mod events;
mod extensions;
mod identity;
mod journal;
mod leases;
mod lifecycle;
mod links;
mod locality;
mod nodes;
mod observe;
mod offload_expiry;
mod offloads;
mod operator;
mod pebble;
mod pin;
mod placement;
mod plans;
mod provider_breaker;
mod provider_calls;
mod providers;
mod proxy;
mod prunes;
mod quota;
mod reads;
mod recipes;
mod relay;
mod relays;
mod renewal;
mod replace;
mod repositories;
mod restores;
mod resume;
mod retire;
mod revocation;
mod routes;
mod sdk;
mod served_target;
mod session;
mod sets;
mod shortlist;
mod snapshots;
mod standalone;
mod step;
mod step_pin;
mod step_resolve;
mod stream_share;
mod streams;
mod support;
mod tenancy;
mod tenant_keys;
mod tokens;
mod told;
mod trust;
mod upgrade;
mod volume_attach;
mod volume_delete;
mod volumes;
mod watch;
mod workload;
mod zone;
