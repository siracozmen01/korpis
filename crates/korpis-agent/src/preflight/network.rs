// SPDX-License-Identifier: AGPL-3.0-or-later

//! What workloads' networks need (§4, §4.1, and §7 of docs/07-networking.md, §2 of
//! docs/18-operations.md): nftables 1.0 or later with its JSON input, IP forwarding, IPv6 for
//! the default addressing mode, and `tc` with the HTB qdisc for bandwidth. A node without the
//! first two gives its workloads loopback alone; one without IPv6 addresses them over IPv4 only;
//! one that cannot shape enforces no bandwidth.

use super::{Check, Machine, Outcome, Probes};

/// The checks of the network.
#[must_use]
pub fn checks(m: &Machine, p: &Probes) -> Vec<Check> {
    vec![nftables(p), forwarding(m), ipv6(m), shaping(p)]
}

/// `major.minor` of what `nft --version` prints, e.g. `nftables v1.0.6 (Lester Gooch #5)`.
fn nft_version(printed: &str) -> Option<(u32, u32)> {
    let v = printed.split_whitespace().find_map(|w| w.strip_prefix('v'))?;
    let mut parts = v.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

fn nftables(p: &Probes) -> Check {
    let name = "nftables 1.0+, JSON";
    let needed_for = "every workload's network";
    let costs = "workloads have loopback alone, and no endpoint is offered on this node";
    let (found, outcome) = match &p.nft {
        None => (
            "not installed".to_owned(),
            Outcome::Lacking {
                costs: costs.into(),
                fix: "install nftables 1.0 or later from the distribution".into(),
            },
        ),
        Some((printed, json)) => {
            let recent = nft_version(printed).is_some_and(|v| v >= (1, 0));
            let outcome = if recent && *json {
                Outcome::Met
            } else {
                Outcome::Lacking {
                    costs: costs.into(),
                    fix: "install nftables 1.0 or later, built with JSON support".into(),
                }
            };
            let json = if *json { "JSON" } else { "no JSON" };
            (format!("{printed}, {json}"), outcome)
        }
    };
    Check { name, needed_for, found, outcome, essential: false }
}

fn forwarding(m: &Machine) -> Check {
    let on = m.proc("sys/net/ipv4/ip_forward").is_some_and(|v| v == "1");
    let outcome = if on {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "workloads have loopback alone, and no endpoint is offered on this node".into(),
            fix: "sysctl -w net.ipv4.ip_forward=1, and the same line in \
                  /etc/sysctl.d/90-korpis.conf"
                .into(),
        }
    };
    let found = if on { "on" } else { "off" }.to_owned();
    Check {
        name: "IP forwarding",
        needed_for: "every workload's network",
        found,
        outcome,
        essential: false,
    }
}

fn ipv6(m: &Machine) -> Check {
    let disabled = m.proc("sys/net/ipv6/conf/all/disable_ipv6");
    let on = disabled.as_deref() == Some("0");
    let outcome = if on {
        Outcome::Met
    } else {
        Outcome::Lacking {
            costs: "IPv4 only, and its scarcity is the operator's to manage".into(),
            fix: if disabled.is_some() {
                "sysctl -w net.ipv6.conf.all.disable_ipv6=0, and the same line in \
                 /etc/sysctl.d/90-korpis.conf"
                    .into()
            } else {
                "a kernel with IPv6, and nothing on its command line disabling it (ipv6.disable=1)"
                    .into()
            },
        }
    };
    let found = if on { "on" } else { "off" }.to_owned();
    Check {
        name: "IPv6",
        needed_for: "the default addressing mode",
        found,
        outcome,
        essential: false,
    }
}

fn shaping(p: &Probes) -> Check {
    let (found, outcome) = match &p.shaping {
        Some(Ok(())) => ("tc and HTB".to_owned(), Outcome::Met),
        Some(Err(why)) => (
            why.clone(),
            Outcome::Lacking {
                costs: "the node enforces no bandwidth, and nothing that sets one is placed here"
                    .into(),
                fix: "install iproute2's tc, with a kernel that has the HTB qdisc, and declare \
                      each uplink with korpis-agent run --uplink eth0=1gbit"
                    .into(),
            },
        ),
        None => ("not asked".to_owned(), Outcome::Unchecked("the helper was not asked".into())),
    };
    Check {
        name: "tc and the HTB qdisc",
        needed_for: "bandwidth reservations and limits",
        found,
        outcome,
        essential: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nft_s_version_is_read_from_what_it_prints() {
        assert_eq!(nft_version("nftables v1.0.6 (Lester Gooch #5)"), Some((1, 0)));
        assert_eq!(nft_version("nftables v0.9.3 (Topsy)"), Some((0, 9)));
        assert_eq!(nft_version("something else"), None);
    }
}
