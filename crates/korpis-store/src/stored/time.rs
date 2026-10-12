// SPDX-License-Identifier: AGPL-3.0-or-later

//! Timestamps: the domain reads `SystemTime`, the database stores `timestamptz`.

use chrono::{DateTime, Utc};
use std::time::SystemTime;

/// A stored timestamp.
pub(crate) fn to_time(t: DateTime<Utc>) -> SystemTime {
    t.into()
}

/// A timestamp to store.
pub(crate) fn from_time(t: SystemTime) -> DateTime<Utc> {
    t.into()
}
