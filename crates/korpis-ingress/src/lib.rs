// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-ingress`, the proxy in front of a node's `ingress` endpoints (§3.2 of
//! docs/07-networking.md). It is the node's most exposed surface, so it is a program of its own,
//! run and fed by the agent: it serves the sockets the agent hands it, with the routes,
//! certificates, and challenges the agent sends, and holds nothing else.

pub mod confine;
pub mod control;
pub mod forward;
pub mod headers;
pub mod proxy_header;
pub mod serve;
pub mod sni;
pub mod stream;
pub mod table;
pub mod tls;
pub mod unverified;

pub use serve::{Listeners, serve};
pub use table::Table;
