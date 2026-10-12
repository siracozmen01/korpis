// SPDX-License-Identifier: Apache-2.0

//! The Korpis wire schema.
//!
//! Everything a third party needs to interoperate with Korpis lives here and nowhere else: the
//! client API, the agent protocol (§4 of `docs/02-architecture.md`), and the internal agent
//! protocols (§6.2 of the same document). It is Apache-2.0 so that a conforming agent or driver
//! can be written without reading or linking the AGPL code (Rule K-7).
//!
//! The `.proto` files under `proto/` are the source; the Rust code is generated at build time.

#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::restriction,
    missing_debug_implementations,
    rust_2018_idioms,
    reason = "generated code is not ours to restyle; the schema is what is reviewed"
)]

connectrpc::include_generated!();
