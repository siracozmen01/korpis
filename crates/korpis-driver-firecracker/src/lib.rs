// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `firecracker` runtime driver (§3 of docs/04-runtimes.md): the `microvm` tier, each guest a
//! Firecracker microVM started by Firecracker's jailer, in a chroot of its own, as one of its
//! workload's own ids, in a cgroup holding its limits. The guest boots the node's kernel and
//! Korpis's first process from the boot drive, its image from a drive built of the image's layers,
//! and what to run from its config drive (§6 there). It is Rule D's second driver: the most
//! different from the `oci` one, sharing with it only what `korpis_driver_kit` holds.
//!
//! The crate has one unsafe block, registering a process's descriptor it owns with tokio's
//! reactor in [`process`], which tokio offers only as an unsafe constructor; it carries the
//! `SAFETY` argument a reviewer checks.

#![warn(missing_docs)]

pub mod agent;
pub mod api;
pub mod console;
pub mod describe;
pub mod guest;
pub mod jail;
pub mod machine;
pub mod object;
pub mod prepare;
pub mod process;
pub mod relay;
pub mod service;
pub mod settings;
pub mod sync;
pub mod volume;
