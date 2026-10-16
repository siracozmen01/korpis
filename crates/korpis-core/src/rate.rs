// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rate limits (§10 of docs/10-api.md): buckets of a generic cell rate, each a steady rate and a
//! burst. A bucket is one instant, the moment it would be full again; a request is admitted when
//! admitting it leaves that instant no further ahead than the burst allows, and moves it on by one
//! request's interval.

use std::fmt;
use std::time::Duration;

/// A steady rate, in requests a second, and the burst a full bucket admits at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// Requests a second, at least one.
    pub per_second: u32,
    /// Requests a full bucket admits at once, at least one.
    pub burst: u32,
}

/// Why a rate cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a rate is <requests a second>/<burst>, each at least 1: {0:?}")]
pub struct RateError(pub String);

impl Rate {
    /// `<per second>/<burst>`, as a flag writes it.
    ///
    /// # Errors
    /// It is not two positive integers so written.
    pub fn parse(s: &str) -> Result<Self, RateError> {
        let bad = || RateError(s.to_owned());
        let (rate, burst) = s.split_once('/').ok_or_else(bad)?;
        let per_second: u32 = rate.trim().parse().map_err(|_| bad())?;
        let burst: u32 = burst.trim().parse().map_err(|_| bad())?;
        if per_second == 0 || burst == 0 {
            return Err(bad());
        }
        Ok(Self { per_second, burst })
    }

    /// The interval one request moves the bucket on by.
    #[must_use]
    pub fn interval(self) -> Duration {
        Duration::from_secs(1) / self.per_second
    }

    /// How long an empty bucket takes to fill.
    #[must_use]
    pub fn window(self) -> Duration {
        self.interval() * self.burst
    }

    /// A tenth of this rate and of its burst, at least one each: the expensive budget beside an
    /// ordinary one.
    #[must_use]
    pub fn tenth(self) -> Self {
        Self { per_second: (self.per_second / 10).max(1), burst: (self.burst / 10).max(1) }
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.per_second, self.burst)
    }
}

/// The rates an installation charges requests to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    /// Each subject's.
    pub individual: Rate,
    /// Each organization's.
    pub organization: Rate,
    /// Each subject's, for expensive requests, beside its own.
    pub expensive_individual: Rate,
    /// Each organization's, for expensive requests, beside its own.
    pub expensive_organization: Rate,
}

impl Default for Rates {
    fn default() -> Self {
        Self {
            individual: Rate { per_second: 20, burst: 200 },
            organization: Rate { per_second: 100, burst: 1_000 },
            expensive_individual: Rate { per_second: 2, burst: 20 },
            expensive_organization: Rate { per_second: 10, burst: 100 },
        }
    }
}

/// One bucket a request is charged to: its policy's name, as response headers show it, the key
/// it is kept under, and its rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    /// The policy: `individual`, `organization`, `expensive-individual`, ….
    pub policy: &'static str,
    /// Where it is kept.
    pub key: String,
    /// Its rate.
    pub rate: Rate,
}

/// Where a bucket stands after a request was charged to it, or refused by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    /// The bucket.
    pub bucket: Bucket,
    /// Whether it admitted the request.
    pub admitted: bool,
    /// How long until it is full again.
    pub full_in: Duration,
}

impl Standing {
    /// The requests it would still admit at once.
    #[must_use]
    pub fn remaining(&self) -> u32 {
        let window = self.bucket.rate.window();
        let left = window.saturating_sub(self.full_in);
        let n = left.as_nanos() / self.bucket.rate.interval().as_nanos().max(1);
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// How long until it admits the next request, if it refused this one.
    #[must_use]
    pub fn retry_after(&self) -> Duration {
        let rate = self.bucket.rate;
        (self.full_in + rate.interval()).saturating_sub(rate.window())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_reads_as_a_flag_writes_it_and_measures_its_bucket() {
        let r = Rate::parse("20/200").unwrap();
        assert_eq!(
            (r.interval(), r.window()),
            (Duration::from_millis(50), Duration::from_secs(10))
        );
        assert_eq!(r.to_string(), "20/200");
        for bad in ["20", "0/5", "5/0", "a/b", "-1/4"] {
            assert!(Rate::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Rate { per_second: 5, burst: 9 }.tenth(), Rate { per_second: 1, burst: 1 });
    }

    #[test]
    fn a_standing_says_what_remains_and_when_to_retry() {
        let bucket = Bucket {
            policy: "individual",
            key: "i:x".into(),
            rate: Rate { per_second: 10, burst: 5 },
        };
        let full = Standing { bucket: bucket.clone(), admitted: true, full_in: Duration::ZERO };
        assert_eq!(full.remaining(), 5);
        let empty = Standing { bucket, admitted: false, full_in: Duration::from_millis(500) };
        assert_eq!(empty.remaining(), 0);
        assert_eq!(empty.retry_after(), Duration::from_millis(100));
    }
}
