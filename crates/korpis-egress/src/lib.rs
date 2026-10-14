// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where Korpis's own outbound requests made on a tenant's behalf may go (§4 of
//! docs/09-recipes.md, §3 of docs/17-security.md). A recipe names URLs and registries, and anyone
//! may write a recipe, so a request to one of them is a request whose destination a tenant chose,
//! made from the node or the control plane, which can reach what the tenant cannot: loopback, the
//! cloud's metadata service, the operator's private network. Every such request reaches only the
//! public internet and the networks the operator names.
//!
//! A client that takes a resolver is given [`Reach`] as its resolver and [`redirects`] as its
//! redirect policy. A client that does not (the OCI registry client) is given [`Proxy`] as its
//! proxy, for HTTPS and HTTP alike, and the proxy applies the same policy to every connection.

pub mod proxy;
pub mod reach;

pub use proxy::Proxy;
pub use reach::{Reach, public, redirects};
