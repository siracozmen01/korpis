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

mod accounts;
mod acme;
mod agent;
mod assignments;
mod authority;
mod authz;
mod backup_read;
mod backup_target;
mod backups;
mod bootstrap;
mod cancel;
mod certificates;
mod churn;
mod classify;
mod compromise;
mod console;
mod control_backup;
mod denials;
mod dependencies;
mod device_grant;
mod devices;
mod dns;
mod domains;
mod effects;
mod egress;
mod endpoints;
mod enroll;
mod epoch_fence;
mod events;
mod extensions;
mod failures;
mod identity;
mod issuance;
mod issuance_dns;
mod journal;
mod leases;
mod lifecycle;
mod links;
mod locality;
mod migrate;
mod names;
mod nodes;
mod observe;
mod offload_expiry;
mod offloads;
mod operator;
mod panel;
mod pebble;
mod pin;
mod placement;
mod plans;
mod profile;
mod provider_breaker;
mod provider_calls;
mod providers;
mod provision;
mod proxy;
mod prunes;
mod quota;
mod rate_grants;
mod rates;
mod reads;
mod recipes;
mod refusal;
mod relay;
mod relay_hold;
mod relay_watch;
mod relays;
mod removal;
mod renewal;
mod replace;
mod repositories;
mod restores;
mod resume;
mod retire;
mod revocation;
mod rotation;
mod routes;
mod sdk;
mod secrets;
mod served_target;
mod session;
mod sessions;
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
mod theme;
mod tokens;
mod told;
mod trust;
mod upgrade;
mod volume_attach;
mod volume_delete;
mod volumes;
mod watch;
mod web;
mod workload;
mod workload_recipe;
mod zone;
