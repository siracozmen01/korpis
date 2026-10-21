// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each endpoint whose accounting is on carried (§2 of docs/15-observability.md, "Bytes by
//! endpoint, and egress"): counted in its node's host table, cumulative in a sample, and the
//! differences in a record, named by the endpoint's name.

use std::collections::BTreeMap;

/// One endpoint's bytes and packets: what was sent to its port, and what left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EndpointUse {
    /// Bytes sent to its port.
    pub received_bytes: u64,
    /// Packets sent to its port.
    pub received_packets: u64,
    /// Bytes that left its port.
    pub sent_bytes: u64,
    /// Packets that left its port.
    pub sent_packets: u64,
}

impl EndpointUse {
    /// Its four counters, in order.
    #[must_use]
    pub const fn fields(&self) -> [u64; 4] {
        [self.received_bytes, self.received_packets, self.sent_bytes, self.sent_packets]
    }

    const fn from_fields(
        [received_bytes, received_packets, sent_bytes, sent_packets]: [u64; 4],
    ) -> Self {
        Self { received_bytes, received_packets, sent_bytes, sent_packets }
    }
}

/// Each endpoint, by name.
pub type Endpoints = BTreeMap<String, EndpointUse>;

/// Whether any endpoint's counter in `now` is below the same in `before`.
#[must_use]
pub fn below(now: &Endpoints, before: &Endpoints) -> bool {
    now.iter().any(|(name, n)| {
        before.get(name).is_some_and(|b| n.fields().iter().zip(b.fields()).any(|(n, b)| *n < b))
    })
}

/// What each endpoint of `now` carried since `before`; one `before` did not count is counted
/// from zero, as a counter first measured is.
#[must_use]
pub fn since(now: &Endpoints, before: &Endpoints) -> Endpoints {
    now.iter()
        .map(|(name, n)| {
            let b = before.get(name).copied().unwrap_or_default();
            let mut out = [0; 4];
            for (o, (n, b)) in out.iter_mut().zip(n.fields().into_iter().zip(b.fields())) {
                *o = n.saturating_sub(b);
            }
            (name.clone(), EndpointUse::from_fields(out))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(name: &str, received_bytes: u64) -> Endpoints {
        let used =
            EndpointUse { received_bytes, sent_bytes: received_bytes * 2, ..Default::default() };
        Endpoints::from([(name.to_owned(), used)])
    }

    #[test]
    fn an_endpoint_is_differenced_by_name_and_one_first_counted_from_zero() {
        let mut now = one("game", 150);
        now.extend(one("map", 5));
        let used = since(&now, &one("game", 100));
        assert_eq!(used["game"].received_bytes, 50);
        assert_eq!(used["game"].sent_bytes, 100);
        assert_eq!(used["map"].received_bytes, 5, "first counted, from zero");
        assert!(below(&one("game", 10), &one("game", 100)), "a count begun again");
        assert!(!below(&one("map", 10), &one("game", 100)), "another endpoint's is not");
    }
}
