// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every runtime driver shares (§3 and §4 of docs/04-runtimes.md): the socket it serves the
//! driver protocol on and who may use it ([`serve`]), the spec it prepares from an intent and its
//! recipe, which differs between drivers only in the tier and name ([`prepare`]), a console's
//! files as the stream writer is handed them ([`console`]), and the counters a cgroup keeps for
//! whatever runs in it ([`counters`]). Rule D's second driver is what
//! showed which parts these are: the `oci` driver held them, and the `firecracker` driver needs
//! them unchanged.

#![warn(missing_docs)]

pub mod console;
pub mod counters;
pub mod entry;
pub mod prepare;
pub mod serve;
