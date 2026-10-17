// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's files, as their owner reaches them through a file session (§3.2 of
//! docs/06-storage.md): the verbs, served beneath the root the filesystem worker is given, and
//! the session's requests answered with them. The worker that confines itself and serves a
//! session over its socket (`korpis-fsworker`) is built on this; nothing here knows where its
//! requests come from.

pub mod frame;
pub mod fs;
pub mod serve;
pub mod volumes;

#[cfg(test)]
mod fs_tests;
