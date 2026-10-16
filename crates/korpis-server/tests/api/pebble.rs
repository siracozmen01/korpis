// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pebble for the API tests, from korpis-server's test support; a test without it says so and
//! does not run.

use korpis_server::testing::{Pebble, PebbleOptions, TestingError};

/// Pebble, every challenge taken as valid when `always_valid`; `None` where it is not installed.
pub async fn start(always_valid: bool) -> Option<Pebble> {
    match Pebble::start(PebbleOptions { always_valid, ..PebbleOptions::default() }).await {
        Ok(p) => Some(p),
        Err(e @ TestingError::NotInstalled(_)) => {
            #[expect(clippy::print_stderr, reason = "a skipped test says why")]
            {
                eprintln!("{e}: not run");
            }
            None
        }
        Err(e) => panic!("pebble: {e}"),
    }
}
