// SPDX-License-Identifier: AGPL-3.0-or-later

//! The job as a node runs it: confinement checked against the kernel, and the binary run with a
//! spec on its standard input, backing up and restoring through a real store.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "a failing test should fail loudly, and a skipped one should say so"
)]

mod confine;
mod run;
