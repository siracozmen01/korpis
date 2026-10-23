// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `oci` runtime driver (§3 of docs/04-runtimes.md): the container tier over containerd.
//! It runs as its own process with access to containerd and nothing else, serves the driver
//! protocol to the agent on a local socket, and treats every spec it receives as hostile until
//! [`validate`] says otherwise, because what it can do with containerd is root (§6.1 of
//! docs/02-architecture.md).

#![warn(missing_docs)]

pub mod containerd;
pub mod describe;
pub mod error;
pub mod oci;
pub mod prepare;
pub mod seccomp;
pub mod service;
pub mod validate;
