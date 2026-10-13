// SPDX-License-Identifier: Apache-2.0

//! containerd's gRPC API (proto/README.md says which version, and where it came from), generated
//! by the same protobuf runtime and RPC stack as the rest of Korpis, so the `oci` driver carries
//! one of each rather than a second pair beside them (§6 of docs/21-stack.md). containerd serves
//! gRPC on a local socket; a client reaches it with [`connectrpc`]'s HTTP/2 connection over a
//! Unix socket and the gRPC protocol, and names its namespace in every request's
//! `containerd-namespace` header.

#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::restriction,
    missing_debug_implementations,
    rust_2018_idioms,
    reason = "generated code is not ours to restyle; the schema is containerd's"
)]

connectrpc::include_generated!();
