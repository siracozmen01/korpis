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

// The module tree; without the `services` feature each service's file is empty (build.rs).
include!(concat!(env!("OUT_DIR"), "/_connectrpc.rs"));

/// The schema as a compiled `FileDescriptorSet`, the same bytes the code above is generated
/// from. Readers that need the schema at run time (to emit a message's default fields, for
/// one) use this rather than a copy that could drift.
pub const DESCRIPTOR_SET: &[u8] = include_bytes!("../descriptor.binpb");
