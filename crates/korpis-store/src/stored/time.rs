// SPDX-License-Identifier: AGPL-3.0-or-later

//! Timestamps: the domain reads `SystemTime`, the database stores `timestamptz`.

use chrono::{DateTime, Utc};
use std::time::{SystemTime, UNIX_EPOCH};

/// A stored timestamp.
pub(crate) fn to_time(t: DateTime<Utc>) -> SystemTime {
    t.into()
}

/// A timestamp to store. chrono's own conversion panics on a time past the year 262143, which a
/// value from outside could carry; this one stores the latest time it can hold instead, and the
/// earliest for one before 1970 so far it cannot be held either.
pub(crate) fn from_time(t: SystemTime) -> DateTime<Utc> {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs())
            .ok()
            .and_then(|s| DateTime::from_timestamp(s, d.subsec_nanos()))
            .unwrap_or(DateTime::<Utc>::MAX_UTC),
        Err(before) => {
            let d = before.duration();
            i64::try_from(d.as_secs())
                .ok()
                .and_then(|s| DateTime::from_timestamp(-s, 0))
                .unwrap_or(DateTime::<Utc>::MIN_UTC)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_time_too_far_out_to_hold_is_clamped_not_a_panic() {
        let far = UNIX_EPOCH + Duration::from_secs(u64::try_from(i64::MAX).unwrap());
        assert_eq!(from_time(far), DateTime::<Utc>::MAX_UTC);
        let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(to_time(from_time(at)), at);
    }
}
