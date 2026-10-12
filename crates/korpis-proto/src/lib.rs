// SPDX-License-Identifier: Apache-2.0

//! The Korpis wire schema.
//!
//! Everything a third party needs to interoperate with Korpis lives here and nowhere else: the
//! client API, the agent protocol (§4 of `docs/02-architecture.md`), and the internal agent
//! protocols (§6.2 of the same document). It is Apache-2.0 so that a conforming agent or driver
//! can be written without reading or linking the AGPL code (Rule K-7).
