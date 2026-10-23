// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's bandwidth (§7 of docs/07-networking.md): the uplinks its operator declares, which
//! the helper shapes; the bandwidth the node then offers placement, the smallest uplink's
//! capacity less what the node keeps of it; and what its drivers that join its links then
//! enforce, since the node's network holds it and not the driver.

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

/// What the node's network holds for the workloads of its link-joining drivers: the kinds of
/// limit it enforces, each with how, and the bandwidth it offers placement with that capacity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Held {
    /// Each kind, as placement asks for it, with the mechanism that holds it.
    pub kinds: Vec<(&'static str, &'static str)>,
    /// What the smallest uplink offers less what the node keeps of it, and its capacity.
    pub bandwidth: Option<(u64, u64)>,
}

/// What the node's network holds once the helper has made its `uplinks`: an endpoint's traffic
/// counted wherever the helper gives workloads networks; an ingress limit and an endpoint's own
/// shaping wherever it can shape, on the workload's link; and an egress reservation or limit
/// only where an uplink is declared and shaped, since a reservation is held there. Each thing
/// the node cannot do is logged with why.
pub async fn held<H: Host>(host: &H, uplinks: &[helper::Uplink]) -> Held {
    let Ok(described) = host.describe_network().await else { return Held::default() };
    if !described.offered || !described.shapes {
        if described.offered && !uplinks.is_empty() {
            tracing::warn!(
                why = described.why_not_shapes,
                "uplinks were declared and this node cannot shape, so it enforces no bandwidth \
                 and nothing that sets one is placed here"
            );
        }
        return of(&described, &[]);
    }
    // Asked even with no uplink declared, so a tree an earlier declaration left is removed.
    let request = helper::EnsureUplinksRequest { uplinks: uplinks.to_vec(), ..Default::default() };
    match host.ensure_uplinks(&request).await {
        Ok(made) => {
            for removed in &made.removed {
                tracing::info!(uplink = removed, "an uplink no longer declared is not shaped");
            }
            of(&described, uplinks)
        }
        Err(e) => {
            tracing::error!(error = %e, "the uplinks could not be shaped; no egress enforced");
            of(&described, &[])
        }
    }
}

/// What a network `described` so holds, with `shaped` the uplinks the helper has made.
fn of(described: &helper::DescribeNetworkResponse, shaped: &[helper::Uplink]) -> Held {
    let mut held = Held::default();
    if !described.offered {
        return held;
    }
    held.kinds.push(("endpoint_accounting", "named counters in the host's nftables table"));
    if !described.shapes {
        return held;
    }
    held.kinds.push(("ingress", "tc htb on the host's side of the workload's link"));
    held.kinds.push(("endpoint_shaping", "tc htb classes on the workload's link, by port"));
    if let Some(smallest) = shaped.iter().min_by_key(|u| u.capacity - u.reserve) {
        held.kinds.push(("egress", "tc htb on the node's uplinks and on the workload's own link"));
        held.bandwidth = Some((smallest.capacity - smallest.reserve, smallest.capacity));
    }
    held
}

/// Declares on `driver`, if it joins the node's links, that it enforces what the node's network
/// holds for it, `kinds` (§4 of docs/04-runtimes.md).
pub fn enforced(driver: &mut pb::DriverInfo, kinds: &[(&str, &str)]) {
    let Some(caps) = driver.capabilities.as_option_mut() else { return };
    if !crate::network::joins_links(caps) {
        return;
    }
    for (kind, mechanism) in kinds {
        let declared = pb::EnforcementDecl {
            enforced: true,
            mechanism: (*mechanism).to_owned(),
            granularity: if kind.ends_with("accounting") { "bytes" } else { "bits a second" }
                .to_owned(),
            ..Default::default()
        };
        caps.enforcement.insert((*kind).to_owned(), declared);
    }
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
    fn each_kind_is_held_only_where_the_network_has_what_it_rests_on() {
        let kinds = |h: &Held| h.kinds.iter().map(|(k, _)| *k).collect::<Vec<_>>();
        let mut d = helper::DescribeNetworkResponse::default();
        assert_eq!(of(&d, &[]), Held::default(), "no networks, nothing held");
        d.offered = true;
        assert_eq!(kinds(&of(&d, &[])), ["endpoint_accounting"], "counted, and not shaped");
        d.shapes = true;
        let unshaped = of(&d, &[]);
        assert_eq!(kinds(&unshaped), ["endpoint_accounting", "ingress", "endpoint_shaping"]);
        assert_eq!(unshaped.bandwidth, None, "no uplink, no reservation, no egress");
        let u = |capacity, reserve| helper::Uplink { capacity, reserve, ..Default::default() };
        let both = of(&d, &[u(1000, 50), u(400, 20)]);
        assert!(kinds(&both).contains(&"egress"));
        assert_eq!(both.bandwidth, Some((380, 400)), "the smallest uplink, less what it keeps");
    }

    #[test]
    fn a_driver_that_joins_the_nodes_links_enforces_what_they_hold_and_another_does_not() {
        let caps = pb::Capabilities {
            network_modes: vec![pb::NetworkMode::NETWORK_MODE_VETH.into()],
            ..Default::default()
        };
        let mut veth = pb::DriverInfo { capabilities: caps.into(), ..Default::default() };
        enforced(&mut veth, &[("egress", "tc"), ("ingress", "tc")]);
        let held = veth.capabilities.as_option().map(|c| c.enforcement.clone()).unwrap_or_default();
        assert!(held.get("egress").is_some_and(|e| e.enforced) && held.contains_key("ingress"));
        let mut own = pb::DriverInfo {
            capabilities: pb::Capabilities::default().into(),
            ..Default::default()
        };
        enforced(&mut own, &[("egress", "tc")]);
        assert!(own.capabilities.as_option().is_some_and(|c| c.enforcement.is_empty()));
        let caps = pb::Capabilities {
            network_modes: vec![pb::NetworkMode::NETWORK_MODE_TAP.into()],
            ..Default::default()
        };
        let mut tap = pb::DriverInfo { capabilities: caps.into(), ..Default::default() };
        enforced(&mut tap, &[("egress", "tc")]);
        let held = tap.capabilities.as_option().map(|c| c.enforcement.clone()).unwrap_or_default();
        assert!(held.get("egress").is_some_and(|e| e.enforced), "a guest's tap is on the link");
    }
}
