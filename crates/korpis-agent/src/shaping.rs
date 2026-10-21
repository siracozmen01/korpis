// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's bandwidth (§7 of docs/07-networking.md): the uplinks its operator declares, which
//! the helper shapes; the bandwidth the node then offers placement, the smallest uplink's
//! capacity less what the node keeps of it; and the limits its drivers that join its links then
//! enforce, since the node's network holds them and not the driver.

use crate::host::Host;
use korpis_core::quantity::parse_rate;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;

/// What the node keeps of an uplink for itself when its operator does not say: five percent.
const RESERVE_PERCENT: u64 = 5;

/// An uplink as `--uplink` declares it: `eth0=1gbit`, or `eth0=1gbit,reserve=50mbit`.
pub fn parse_uplink(text: &str) -> Result<helper::Uplink, String> {
    let bad = |why: &str| format!("{text:?}: {why}; write it as eth0=1gbit[,reserve=50mbit]");
    let (interface, rest) = text.split_once('=').ok_or_else(|| bad("no capacity"))?;
    let (capacity, reserve) = match rest.split_once(",reserve=") {
        Some((c, r)) => (c, Some(r)),
        None => (rest, None),
    };
    let capacity = parse_rate(capacity).map_err(|e| bad(&e.to_string()))?;
    let reserve = match reserve {
        Some(r) => parse_rate(r).map_err(|e| bad(&e.to_string()))?,
        None => capacity / 100 * RESERVE_PERCENT,
    };
    if interface.is_empty() || capacity == 0 || reserve >= capacity {
        return Err(bad("an interface, a capacity, and a reserve below it"));
    }
    Ok(helper::Uplink { interface: interface.to_owned(), capacity, reserve, ..Default::default() })
}

/// The bandwidth the node offers placement once the helper has shaped `uplinks`: the smallest
/// uplink's capacity less what the node keeps of it, and that capacity. None, with why logged,
/// where it shapes nothing.
pub async fn shaped<H: Host>(host: &H, uplinks: &[helper::Uplink]) -> Option<(u64, u64)> {
    let described = host.describe_network().await.ok()?;
    if !described.shapes {
        if !uplinks.is_empty() {
            tracing::warn!(
                why = described.why_not_shapes,
                "uplinks were declared and this node cannot shape them, so it enforces no \
                 bandwidth and nothing that sets one is placed here"
            );
        }
        return None;
    }
    let request = helper::EnsureUplinksRequest { uplinks: uplinks.to_vec(), ..Default::default() };
    match host.ensure_uplinks(&request).await {
        Ok(made) => {
            for removed in &made.removed {
                tracing::info!(uplink = removed, "an uplink no longer declared is not shaped");
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "the uplinks could not be shaped; no bandwidth enforced");
            return None;
        }
    }
    let smallest = uplinks.iter().min_by_key(|u| u.capacity - u.reserve)?;
    Some((smallest.capacity - smallest.reserve, smallest.capacity))
}

/// Declares on `driver`, if it joins the node's links, that it enforces the egress and ingress
/// limits the node's network holds for it (§4 of docs/04-runtimes.md).
pub fn enforced(driver: &mut pb::DriverInfo) {
    let Some(caps) = driver.capabilities.as_option_mut() else { return };
    let joins =
        caps.network_modes.iter().any(|m| m.as_known() == Some(pb::NetworkMode::NETWORK_MODE_VETH));
    if !joins {
        return;
    }
    let declared = |mechanism: &str| pb::EnforcementDecl {
        enforced: true,
        mechanism: mechanism.to_owned(),
        granularity: "bits a second".to_owned(),
        ..Default::default()
    };
    caps.enforcement.insert(
        "egress".to_owned(),
        declared("tc htb on the node's uplinks and on the workload's own link"),
    );
    caps.enforcement
        .insert("ingress".to_owned(), declared("tc htb on the host's side of the workload's link"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uplink_is_an_interface_its_capacity_and_what_the_node_keeps_of_it() {
        let u = parse_uplink("eth0=1gbit").unwrap();
        assert_eq!(
            (u.interface.as_str(), u.capacity, u.reserve),
            ("eth0", 1_000_000_000, 50_000_000)
        );
        let kept = parse_uplink("enp3s0=10gbit,reserve=1gbit").unwrap();
        assert_eq!(kept.reserve, 1_000_000_000);
        assert!(parse_uplink("eth0").is_err() && parse_uplink("eth0=1gbit,reserve=2gbit").is_err());
        assert!(parse_uplink("=1gbit").is_err());
    }

    #[test]
    fn a_driver_that_joins_the_nodes_links_enforces_what_they_hold_and_another_does_not() {
        let caps = pb::Capabilities {
            network_modes: vec![pb::NetworkMode::NETWORK_MODE_VETH.into()],
            ..Default::default()
        };
        let mut veth = pb::DriverInfo { capabilities: caps.into(), ..Default::default() };
        enforced(&mut veth);
        let held = veth.capabilities.as_option().map(|c| c.enforcement.clone()).unwrap_or_default();
        assert!(held.get("egress").is_some_and(|e| e.enforced) && held.contains_key("ingress"));
        let mut own = pb::DriverInfo {
            capabilities: pb::Capabilities::default().into(),
            ..Default::default()
        };
        enforced(&mut own);
        assert!(own.capabilities.as_option().is_some_and(|c| c.enforcement.is_empty()));
    }
}
