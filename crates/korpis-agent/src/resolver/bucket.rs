// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's resolver rate: a token bucket, so a workload's flood of queries is refused
//! without slowing another's, whose resolver has its own.

use korpis_core::workload::network::{RESOLVER_BURST, RESOLVER_RATE};
use std::time::Instant;

/// One workload's bucket.
#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    tokens: f64,
    at: Option<Instant>,
}

impl Default for Bucket {
    fn default() -> Self {
        Self { tokens: f64::from(RESOLVER_BURST), at: None }
    }
}

impl Bucket {
    /// Whether a query may be answered now, taking a token if so.
    pub fn take(&mut self, now: Instant) -> bool {
        let elapsed = self.at.map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f64());
        let refilled = self.tokens + f64::from(RESOLVER_RATE) * elapsed;
        self.tokens = refilled.min(f64::from(RESOLVER_BURST));
        self.at = Some(now);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}
