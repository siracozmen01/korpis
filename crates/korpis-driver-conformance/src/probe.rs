// SPDX-License-Identifier: Apache-2.0

//! What the probe is asked to do, as the entry of the object it runs as
//! (`korpis-conformance-probe`).

/// Runs until the terminating signal, then exits 0.
pub const SERVE: [&str; 2] = ["/probe", "serve"];

/// Runs until it is killed: the terminating signal is taken and ignored.
pub const STUBBORN: [&str; 2] = ["/probe", "stubborn"];

/// Exits at once with status 3.
pub const EXIT_3: [&str; 3] = ["/probe", "exit", "3"];
