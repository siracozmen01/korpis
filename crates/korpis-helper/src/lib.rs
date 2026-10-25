// SPDX-License-Identifier: AGPL-3.0-or-later

//! The privileged helper (§6 of docs/02-architecture.md): the only component of the agent that
//! holds root. It is synchronous and never starts a second thread (§4 of docs/21-stack.md), it
//! serves the supervisor alone, one structured request at a time (`korpis.helper.v1`), and every
//! path it acts on is built from an identifier it checked, never taken from a request.
//!
//! The crate has four unsafe modules, [`quota`], [`btrfs`], and the network's namespaces and
//! taps: the project-quota calls, the btrfs ioctls, `unshare(2)`, and the tun ioctls have no
//! safe wrapper. Every unsafe block in them carries the `SAFETY` argument a reviewer checks.

#![warn(missing_docs)]

pub mod block;
pub mod btrfs;
pub mod cores;
pub mod error;
pub mod files;
pub mod frame;
pub mod helper;
pub mod ids;
pub mod ingress;
pub mod job;
pub mod kept;
pub mod network;
pub mod overlay;
pub mod projects;
pub mod quota;
pub mod serve;
pub mod snapshot;
pub mod staging;
pub mod storage;
pub mod volume;
