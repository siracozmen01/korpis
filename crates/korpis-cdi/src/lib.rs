// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's Container Device Interface spec files, read as its own configuration (§6.1 and §6.5
//! of docs/05-scheduling.md): by the agent, which reports the devices they describe, and by a
//! driver, which gives a workload only a device it finds described itself, with the edits it
//! reads itself, so a supervisor that is compromised can name a device the host described and
//! never an edit (§4.2 of docs/04-runtimes.md).

pub mod file;
mod found;
pub mod spec;

pub use found::{Described, Found, Outcome, describe};

#[cfg(test)]
mod tests;
