// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a workload's placement held on its node, for the time it held it (§2 of
//! docs/15-observability.md): each device, by class, for the seconds it was held, whatever ran on
//! it, integrated by the node from its readings, cumulative in a sample, and the differences in a
//! record. Dedicated cores are one counter beside the others.

use crate::node::driver::DeviceClass;
use std::collections::BTreeMap;

/// The seconds devices of each class were held: two held for an hour are 7,200.
pub type DeviceSeconds = BTreeMap<DeviceClass, u64>;

/// Whether any class's seconds in `now` are below the same in `before`.
#[must_use]
pub fn below(now: &DeviceSeconds, before: &DeviceSeconds) -> bool {
    now.iter().any(|(class, n)| before.get(class).is_some_and(|b| n < b))
}

/// The seconds each class of `now` was held since `before`; one `before` did not count is counted
/// from zero, as a counter first measured is.
#[must_use]
pub fn since(now: &DeviceSeconds, before: &DeviceSeconds) -> DeviceSeconds {
    now.iter()
        .map(|(class, n)| (*class, n.saturating_sub(before.get(class).copied().unwrap_or(0))))
        .collect()
}
