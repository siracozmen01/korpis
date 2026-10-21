// SPDX-License-Identifier: AGPL-3.0-or-later

//! What `korpis-server serve`'s flags are read as, where reading one is more than parsing a
//! number.

use crate::ServeError;
use korpis_core::schedule::PlacementPolicy;
use std::time::Duration;

/// A placement policy from its JSON form, a `korpis.v1.PlacementPolicy`.
pub fn placement_policy(json: &str) -> Result<PlacementPolicy, ServeError> {
    let message: korpis_proto::korpis::v1::PlacementPolicy = serde_json::from_str(json)
        .map_err(|e| ServeError::Config(format!("placement policy: {e}")))?;
    korpis_wire::placement::policy_from_pb("placement_policy", &message)
        .map_err(|e| ServeError::Config(format!("{}: {}", e.field, e.message)))
}

/// A span of time as a flag writes it: a whole number of minutes, hours, or days, `<n>m`,
/// `<n>h`, or `<n>d`, at least a minute and at most a year.
///
/// # Errors
/// It is not so written, or is out of that range.
pub fn span(s: &str) -> Result<Duration, String> {
    let bad = || format!("{s:?}: a span is <n>m, <n>h, or <n>d, from a minute to a year");
    let (n, unit) = s.split_at_checked(s.len().saturating_sub(1)).ok_or_else(bad)?;
    let n: u64 = n.parse().map_err(|_| bad())?;
    let seconds = match unit {
        "m" => n.checked_mul(60),
        "h" => n.checked_mul(3600),
        "d" => n.checked_mul(86_400),
        _ => None,
    };
    seconds.filter(|s| (60..=365 * 86_400).contains(s)).map(Duration::from_secs).ok_or_else(bad)
}

/// A metering interval: a span of whole minutes from one to sixty that divides an hour, so every
/// interval of a day begins on the same minutes of every hour (§2 of docs/15-observability.md).
pub fn metering_interval(s: &str) -> Result<Duration, String> {
    let d = span(s)?;
    let seconds = d.as_secs();
    let fits = seconds <= 3600 && seconds % 60 == 0 && 3600 % seconds == 0;
    fits.then_some(d).ok_or_else(|| format!("{s:?}: a metering interval divides an hour"))
}

/// A metering lateness window: a span of whole hours (§2 of docs/15-observability.md).
pub fn metering_lateness(s: &str) -> Result<Duration, String> {
    let d = span(s)?;
    (d.as_secs() % 3600 == 0)
        .then_some(d)
        .ok_or_else(|| format!("{s:?}: a lateness window is whole hours"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lateness_window_is_whole_hours() {
        assert_eq!(metering_lateness("1d"), Ok(Duration::from_secs(86_400)));
        assert_eq!(metering_lateness("6h"), Ok(Duration::from_secs(6 * 3600)));
        assert!(metering_lateness("90m").is_err());
    }

    #[test]
    fn a_metering_interval_is_whole_minutes_that_divide_an_hour() {
        assert_eq!(metering_interval("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(metering_interval("15m"), Ok(Duration::from_secs(900)));
        for bad in ["7m", "2h", "1d", "0m"] {
            assert!(metering_interval(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_span_is_minutes_hours_or_days_from_a_minute_to_a_year() {
        assert_eq!(span("12h"), Ok(Duration::from_secs(12 * 3600)));
        assert_eq!(span("7d"), Ok(Duration::from_secs(7 * 86_400)));
        assert_eq!(span("1m"), Ok(Duration::from_secs(60)));
        for bad in ["0m", "366d", "12", "h", "", "1.5h", "-1h", "99999999999999999999d"] {
            assert!(span(bad).is_err(), "{bad}");
        }
    }
}
