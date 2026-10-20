// SPDX-License-Identifier: AGPL-3.0-or-later

//! Accounts (§9.6 of docs/08-identity.md), through the use cases against the real schema: issued
//! to a new user, redeemed with a passkey or a password and TOTP, signed in with each, and their
//! factors managed by their holder; and an identity binding read as evaluation finds it. Needs the
//! development database (`scripts/dev-pg.sh up`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod bindings;
mod device_grant;
mod devices;
mod manage;
mod passkeys;
mod passwords;
mod person;
mod recent;
mod root;
mod sessions;
mod support;
