// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether the workload's root is ready: the first process has entered it and, for a workload
//! with an identity directory, its files have arrived. The agent listens from before then, so a
//! driver told the run has started can reach it at once, and a command or a file session asked
//! for meanwhile waits here rather than run in the boot drive's root, where neither belongs.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

static READY: Mutex<bool> = Mutex::new(false);
static WOKEN: Condvar = Condvar::new();

/// How long a command or a session waits for the root: longer than the first sync's own bound.
pub const BOUND: Duration = Duration::from_secs(60);

/// Says the root is ready, waking whatever waits for it.
pub fn set() {
    *READY.lock().unwrap_or_else(PoisonError::into_inner) = true;
    WOKEN.notify_all();
}

/// Whether the root is ready, waited for until [`BOUND`] has passed.
pub fn wait() -> bool {
    let until = Instant::now() + BOUND;
    let mut ready = READY.lock().unwrap_or_else(PoisonError::into_inner);
    while !*ready {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        ready = WOKEN.wait_timeout(ready, left).unwrap_or_else(PoisonError::into_inner).0;
    }
    true
}
