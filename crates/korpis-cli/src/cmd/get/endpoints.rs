// SPDX-License-Identifier: AGPL-3.0-or-later

//! The endpoints `korpis status` shows: each with where a client reaches it, and for a `stable`
//! one the control plane does not know reached, why, with its edge group (§3.1 of
//! docs/07-networking.md).

use crate::show::short;
use korpis_proto::korpis::v1 as pb;
use std::fmt::Write as _;

/// One line per endpoint: its name, its exposure, and where a client reaches it, if anywhere,
/// with why a `stable` one is not known to be reached there and its edge group.
pub(super) fn endpoints(out: &mut String, s: &pb::WorkloadStatus) {
    for e in &s.endpoints {
        let exposure = short(e.exposure, "EXPOSURE_");
        let on_node = matches!(
            e.exposure.as_known(),
            Some(pb::Exposure::EXPOSURE_NODE_PORT | pb::Exposure::EXPOSURE_NODE_BOUND)
        );
        let at = e.address.as_option().map_or_else(
            || if on_node { "no free port on its node".to_owned() } else { "-".to_owned() },
            |a| {
                let at: Vec<String> = a.ips.iter().map(|ip| format!("{ip}:{}", a.port)).collect();
                let at = if at.is_empty() { format!("port {}", a.port) } else { at.join(", ") };
                let why = &a.unreached;
                if why.is_empty() {
                    at
                } else {
                    format!("{at}, unreached: {why} ({})", a.edge_group)
                }
            },
        );
        let _ = writeln!(out, "endpoint:  {} {exposure} {at}", e.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stable_endpoint_not_known_reached_says_why_and_names_its_group() {
        let address = pb::Address {
            ips: vec!["198.51.100.2".into()],
            port: 25_565,
            edge_group: "egr_1".into(),
            unreached: "no member of its edge group holds its address".into(),
            ..Default::default()
        };
        let game = pb::Endpoint {
            name: "game".into(),
            exposure: pb::Exposure::EXPOSURE_STABLE.into(),
            address: address.into(),
            ..Default::default()
        };
        let s = pb::WorkloadStatus { endpoints: vec![game], ..Default::default() };
        let mut out = String::new();
        endpoints(&mut out, &s);
        assert!(out.contains("198.51.100.2:25565, unreached: no member"), "{out}");
        assert!(out.contains("(egr_1)"), "{out}");
    }
}
