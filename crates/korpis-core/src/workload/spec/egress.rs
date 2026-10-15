// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a workload may connect out, beyond the egress default (§6 of docs/07-networking.md).
//!
//! It is the workload's and not an endpoint's: a connection a workload opens leaves from no
//! endpoint, so a node judging the packet has no endpoint to read a rule from. An endpoint's own
//! policy says who may connect to it.

use super::SpecError;
use super::endpoints::{ConntrackLimits, NetworkRule, check_rules};

/// The egress default's limits on a workload: tracked connections, in both directions.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 4096;
/// The egress default's limits on a workload: new connections a second it may open.
pub const DEFAULT_NEW_PER_SEC: u32 = 200;
/// The egress default's limits on a workload: the burst above its rate.
pub const DEFAULT_BURST: u32 = 400;

/// A workload's outbound policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    /// Rules, in order; the first that matches decides. A deny holds anywhere; an allow lets the
    /// connection on to the egress default, which decides the rest.
    pub rules: Vec<NetworkRule>,
    /// Limits on the connections it opens; each may lower the default and not raise it.
    pub limits: ConntrackLimits,
}

impl EgressPolicy {
    /// Checks its rules.
    pub fn validate(&self) -> Result<(), SpecError> {
        check_rules("egress.rules", &self.rules)
    }
}
