// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a declaration may ask of the network, and of its resources, that a node of this release
//! enforces (Rule K-3). A field nothing would enforce is refused when it is declared, naming the
//! phase that builds it or why it is not one a packet filter can hold, rather than accepted,
//! stored, and shown as if it held (§6 and §7 of docs/07-networking.md, §5.2 of
//! docs/05-scheduling.md, §4 of docs/20-roadmap.md).

use crate::error::AppError;
use korpis_core::workload::IntentBody;
use korpis_core::workload::spec::ResourceSpec;
use korpis_core::workload::spec::egress::{DEFAULT_MAX_CONNECTIONS, DEFAULT_NEW_PER_SEC};
use korpis_core::workload::spec::endpoints::{
    ConntrackLimits, EndpointSpec, Exposure, NetworkRule, Peer,
};
use korpis_core::workload::spec::resources::CpuIsolation;

/// Refuses what no node of this release would enforce in `body`: its endpoints' and its own.
pub(super) fn refuse_unenforced(body: &IntentBody) -> Result<(), AppError> {
    for (i, e) in body.endpoints.iter().enumerate() {
        endpoint(&format!("body.endpoints[{i}]"), e)?;
    }
    for (j, r) in body.egress.rules.iter().enumerate() {
        peer(&format!("body.egress.rules[{j}]"), r)?;
    }
    limits(body.egress.limits)?;
    resources(&body.resources)?;
    devices(body)
}

/// Refuses devices until placement holds them and a node gives them (§6 of
/// docs/05-scheduling.md): a request no node of this release would honour is refused rather than
/// stored as asked.
fn devices(body: &IntentBody) -> Result<(), AppError> {
    if body.devices.is_empty() {
        return Ok(());
    }
    Err(AppError::NotYet { what: "devices", phase: 3 })
}

/// Refuses resources no node of this release holds: whole cores pinned need a cpuset and the
/// scheduler's accounting of whole cores, siblings included, which no driver declares yet.
fn resources(r: &ResourceSpec) -> Result<(), AppError> {
    if r.cpu.isolation == CpuIsolation::DedicatedCores {
        return Err(AppError::NotYet { what: "dedicated cores", phase: 3 });
    }
    Ok(())
}

/// Refuses a workload's own limits that raise the default's.
fn limits(limits: ConntrackLimits) -> Result<(), AppError> {
    half_open("body.egress.limits", limits)?;
    if limits.max_connections > DEFAULT_MAX_CONNECTIONS || limits.new_per_sec > DEFAULT_NEW_PER_SEC
    {
        return Err(AppError::invalid(
            "body.egress.limits",
            format!(
                "a workload's limits may lower the default of {DEFAULT_MAX_CONNECTIONS} \
                 connections and {DEFAULT_NEW_PER_SEC} new a second, and not raise it: that \
                 protects every other workload on the node, and only an operator-level policy \
                 could relax it (question 7 of §11 of docs/07-networking.md)"
            ),
        ));
    }
    Ok(())
}

fn half_open(at: &str, limits: ConntrackLimits) -> Result<(), AppError> {
    if limits.max_half_open == 0 {
        return Ok(());
    }
    Err(AppError::invalid(
        format!("{at}.max_half_open"),
        "half-open connections have no counter of their own: the new-connection rate and SYN \
         cookies bound them (§6 of docs/07-networking.md)",
    ))
}

fn endpoint(at: &str, e: &EndpointSpec) -> Result<(), AppError> {
    // An address of a workload's own needs a routed address (§3 of docs/07-networking.md).
    if e.exposure == Exposure::DedicatedIp {
        return Err(AppError::NotYet { what: "dedicated addresses", phase: 3 });
    }
    half_open(&format!("{at}.policy.conntrack"), e.policy.conntrack)?;
    for (j, r) in e.policy.ingress.iter().enumerate() {
        let here = format!("{at}.policy.ingress[{j}]");
        if !r.ports.is_empty() || r.protocol.is_some() {
            return Err(AppError::invalid(
                here,
                "an endpoint's ingress rules say who connects to its own port and protocol",
            ));
        }
        peer(&here, r)?;
    }
    Ok(())
}

fn peer(at: &str, r: &NetworkRule) -> Result<(), AppError> {
    match &r.peer {
        Peer::Cidr(_) => Ok(()),
        Peer::Domain(_) => Err(AppError::invalid(
            format!("{at}.peer"),
            "a packet filter sees addresses, not names, and what a name resolves to changes \
             under it, so a rule naming a domain is not yet one a node can hold (question 8 of \
             §11 of docs/07-networking.md)",
        )),
        // On one node another workload is denied by the default and opened by a dependency, so
        // a rule naming workloads by label decides nothing those do not; it is needed where
        // workloads on other nodes are reached, which the overlay brings (§6 of
        // docs/07-networking.md).
        Peer::Workloads(_) => Err(AppError::NotYet { what: "rules naming workloads", phase: 3 }),
    }
}

#[cfg(test)]
mod tests;
