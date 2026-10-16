// SPDX-License-Identifier: AGPL-3.0-or-later

//! Test support, behind the `testing` feature: Let's Encrypt's test ACME server, Pebble, and its
//! challenge test server's DNS, as scripts/test-tools.sh installs them, each run for one test on
//! ports of its own and stopped when dropped.

mod pebble;

pub use pebble::{ChallengeDns, Pebble, PebbleOptions, TestingError};
