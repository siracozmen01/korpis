// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload uses now: what the meter's latest reading came to for it, published at every
//! reading for a session watching it (the `metering.read` session of [`crate::data::usage`]),
//! and the rates two readings come to. Nothing keeps it; what is billed is the interval's sample.

use super::book::at;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

/// One workload at one reading.
#[derive(Debug, Clone, PartialEq)]
pub struct Now {
    /// When the meter read it.
    pub at: SystemTime,
    /// Its counters since it was first metered, as its samples carry them.
    pub counters: agent::MeterCounters,
    /// The memory its objects held, where any was read.
    pub memory_bytes: Option<u64>,
}

/// Every workload the meter read at its latest reading.
pub type Readings = Arc<BTreeMap<String, Now>>;

/// `after - before` a second of `seconds`, scaled by `scale`; absent where either is.
fn rate(before: Option<u64>, after: Option<u64>, scale: u128, millis: u128) -> Option<u64> {
    let grew = after?.checked_sub(before?)?;
    u64::try_from(u128::from(grew) * scale / millis.max(1)).ok()
}

/// What `after` comes to as a message, its rates over the window from `before`.
#[must_use]
pub fn usage_now(before: Option<&Now>, after: &Now) -> pb::UsageNow {
    let mut now = pb::UsageNow {
        at: at(after.at).into(),
        memory_bytes: after.memory_bytes,
        ..Default::default()
    };
    let Some(before) = before else { return now };
    let Ok(window) = after.at.duration_since(before.at) else { return now };
    let millis = window.as_millis();
    if millis == 0 {
        return now;
    }
    let (b, a) = (&before.counters, &after.counters);
    now.since = at(before.at).into();
    // Microseconds of CPU a millisecond are thousandths of a core; a second's bytes are a
    // millisecond's times a thousand.
    now.cpu_millicores = rate(b.cpu_usec, a.cpu_usec, 1, millis);
    now.cpu_throttled_permille = rate(b.cpu_throttled_usec, a.cpu_throttled_usec, 1, millis);
    now.io_read_bytes_per_second = rate(b.io_read_bytes, a.io_read_bytes, 1000, millis);
    now.io_write_bytes_per_second = rate(b.io_write_bytes, a.io_write_bytes, 1000, millis);
    now.net_rx_bytes_per_second = rate(b.net_rx_bytes, a.net_rx_bytes, 1000, millis);
    now.net_tx_bytes_per_second = rate(b.net_tx_bytes, a.net_tx_bytes, 1000, millis);
    now
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn now(secs: u64, cpu: Option<u64>, rx: Option<u64>) -> Now {
        Now {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            counters: agent::MeterCounters {
                cpu_usec: cpu,
                cpu_throttled_usec: cpu.map(|c| c / 10),
                net_rx_bytes: rx,
                ..Default::default()
            },
            memory_bytes: Some(4096),
        }
    }

    #[test]
    fn rates_are_over_the_window_between_two_readings_and_absent_where_unmeasured() {
        let first = usage_now(None, &now(100, Some(0), None));
        assert_eq!(first.memory_bytes, Some(4096));
        assert_eq!(first.cpu_millicores, None, "the first reading has nothing before it");
        assert!(first.since.as_option().is_none());

        // Five seconds of CPU over ten seconds is half a core, throttled a twentieth of it.
        let later = usage_now(Some(&now(100, Some(0), None)), &now(110, Some(5_000_000), None));
        assert_eq!(later.cpu_millicores, Some(500));
        assert_eq!(later.cpu_throttled_permille, Some(50));
        assert_eq!(later.net_rx_bytes_per_second, None, "no interface read");
        assert_eq!(later.since.as_option().map(|t| t.seconds), Some(100));

        let net = usage_now(Some(&now(0, None, Some(1000))), &now(10, None, Some(21_000)));
        assert_eq!(net.net_rx_bytes_per_second, Some(2000));
        assert_eq!(net.cpu_millicores, None);
        let back = usage_now(Some(&now(0, Some(9), None)), &now(10, Some(1), None));
        assert_eq!(back.cpu_millicores, None, "a counter that went back is not a rate");
    }
}
