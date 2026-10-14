// SPDX-License-Identifier: AGPL-3.0-or-later

//! The limits of a workload's console stream (§5 of docs/14-streams.md): how fast it may write
//! before what it writes is dropped and recorded as a gap, and how much of it is kept. They are
//! enforced where the stream is written, and the most it can hold counts against the workload's
//! disk quota like a volume it creates (K-3), because logs that fill a disk are a disk.

use super::SpecError;
use std::time::Duration;

/// The largest record a stream keeps (question 5 of §9 of docs/14-streams.md).
pub const MAX_RECORD: u64 = 64 * 1024;
/// The size at which a segment is sealed.
pub const SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
/// How long an offloaded segment is kept unless the workload says otherwise: a month of
/// evidence, as the console's own history of an incident is wanted weeks after it.
pub const OFFLOAD_RETENTION: Duration = Duration::from_secs(30 * 86_400);

/// How much of a stream is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    /// Sealed segments beyond this many bytes are pruned, oldest first.
    Bytes(u64),
    /// Sealed segments older than this are pruned.
    Duration(Duration),
}

/// A workload's stream limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamSpec {
    /// Bytes a second admitted on average.
    pub rate: u64,
    /// Bytes admitted at once after a quiet period.
    pub burst: u64,
    /// How much is kept on the node.
    pub retention: Retention,
    /// How long an offloaded segment is kept, from when it was offloaded (§4 of
    /// docs/14-streams.md).
    pub offload_retention: Duration,
}

impl Default for StreamSpec {
    /// 256 KiB a second with a burst of 4 MiB, keeping 64 MiB: days of a game server's console.
    fn default() -> Self {
        Self {
            rate: 256 * 1024,
            burst: 4 * 1024 * 1024,
            retention: Retention::Bytes(64 * 1024 * 1024),
            offload_retention: OFFLOAD_RETENTION,
        }
    }
}

impl StreamSpec {
    /// Checks the limits can admit a record at all and keep a segment.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.rate == 0 {
            return Err(SpecError::new("streams.rate_bytes_per_sec", "more than zero"));
        }
        if self.burst < MAX_RECORD {
            return Err(SpecError::new(
                "streams.burst_bytes",
                "at least one largest record, 64 KiB, or such a record is never admitted",
            ));
        }
        match self.retention {
            Retention::Bytes(b) if b < SEGMENT_BYTES => Err(SpecError::new(
                "streams.retention_bytes",
                "at least one segment, 8 MiB, or every sealed segment is pruned at once",
            )),
            Retention::Duration(d) if d.is_zero() => {
                Err(SpecError::new("streams.retention_duration", "more than zero"))
            }
            _ if self.offload_retention.is_zero() => {
                Err(SpecError::new("streams.offload_retention", "more than zero"))
            }
            _ => Ok(()),
        }
    }

    /// How long an offloaded segment is kept under an operator floor of `floor`: what the
    /// workload declares, or the floor where that is longer (§8 of docs/17-security.md).
    #[must_use]
    pub fn offload_kept(&self, floor: Duration) -> Duration {
        self.offload_retention.max(floor)
    }

    /// The most the stream can hold on disk: what its retention keeps, the segment still being
    /// written, and, for a retention by age, all its rate can write in that time.
    #[must_use]
    pub fn disk_bound(&self) -> u64 {
        let kept = match self.retention {
            Retention::Bytes(b) => b,
            Retention::Duration(d) => {
                let seconds = d.as_secs().saturating_add(u64::from(d.subsec_nanos() > 0));
                self.rate.saturating_mul(seconds).saturating_add(self.burst)
            }
        };
        kept.saturating_add(SEGMENT_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_that_could_admit_nothing_are_refused_and_the_bound_counts_every_byte() {
        assert!(StreamSpec::default().validate().is_ok());
        assert_eq!(StreamSpec::default().disk_bound(), 72 * 1024 * 1024);
        let tight = StreamSpec { burst: 1024, ..StreamSpec::default() };
        assert_eq!(tight.validate().unwrap_err().field, "streams.burst_bytes");
        let small = StreamSpec { retention: Retention::Bytes(1024), ..StreamSpec::default() };
        assert!(small.validate().is_err());
        let day = StreamSpec {
            rate: 1000,
            burst: MAX_RECORD,
            retention: Retention::Duration(Duration::from_secs(86_400)),
            offload_retention: Duration::from_secs(86_400),
        };
        assert_eq!(day.disk_bound(), 86_400_000 + MAX_RECORD + SEGMENT_BYTES);
        assert_eq!(day.offload_kept(Duration::from_secs(7 * 86_400)).as_secs(), 7 * 86_400);
        assert_eq!(day.offload_kept(Duration::ZERO).as_secs(), 86_400, "no floor");
        let none = StreamSpec { offload_retention: Duration::ZERO, ..StreamSpec::default() };
        assert_eq!(none.validate().unwrap_err().field, "streams.offload_retention");
    }
}
