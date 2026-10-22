// SPDX-License-Identifier: AGPL-3.0-or-later

//! The proxy end to end on loopback (§3.2 of docs/07-networking.md): real listeners, real TLS,
//! and real workloads behind it, served from a table the test publishes as the agent would.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod alpn;
mod clients;
mod control;
mod passthrough;
mod plain;
mod support;
mod terminated;
mod websocket_h2;
