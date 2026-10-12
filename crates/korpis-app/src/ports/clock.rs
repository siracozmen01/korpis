// SPDX-License-Identifier: AGPL-3.0-or-later

//! The time, as a port: conditions, expiries and token lifetimes are evaluated at an instant a
//! test can choose.

use std::time::SystemTime;

/// A source of the current time.
pub trait Clock: Send + Sync + 'static {
    /// Now.
    fn now(&self) -> SystemTime;
}

/// The system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}
