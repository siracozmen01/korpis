// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis edge-group …` (§3.1 of docs/07-networking.md): the nodes of an organization that hold
//! the stable addresses of its `stable` endpoints, and forward them to wherever each workload
//! runs.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{table, time};
use clap::{Args, Subcommand};
use korpis_proto::korpis::v1 as pb;

/// How fast a group fails over, and which ports it gives; each left out keeps what it is, or its
/// default for a new group.
#[derive(Debug, Clone, Default, Args)]
pub struct Settings {
    /// The ports given to `stable` endpoints, e.g. `20000-40000`; 1024-65534 by default.
    #[arg(long)]
    ports: Option<String>,
    /// The port every member forwards for the control plane's probe; 65535 by default.
    #[arg(long)]
    probe_port: Option<u32>,
    /// Between two probes of a member, e.g. `500ms`; a second by default.
    #[arg(long)]
    probe_interval: Option<String>,
    /// Probes a member misses in a row before it is failing; three by default.
    #[arg(long)]
    probe_misses: Option<u32>,
    /// A member's lease on an address, e.g. `10s`, at least three seconds; ten by default.
    #[arg(long)]
    term: Option<String>,
}

/// `korpis edge-group …`.
#[derive(Debug, Clone, Subcommand)]
pub enum EdgeGroupAction {
    /// Makes a group of nodes of the organization's own; two or more serve its `stable`
    /// endpoints, once an address pool names the group (`korpis address-pool create
    /// --edge-group`).
    Create {
        /// Its name, unique within its organization.
        name: String,
        /// The organization whose group it is, by name or identifier; the top one you can read
        /// by default.
        #[arg(long)]
        organization: Option<String>,
        /// A member, `nd_…`; repeat for more.
        #[arg(long = "member")]
        members: Vec<String>,
        /// How fast it fails over, and which ports it gives.
        #[command(flatten)]
        settings: Settings,
    },
    /// Shows a group: its members and settings.
    Get {
        /// The group, `egr_…`.
        group: String,
    },
    /// The groups of an organization.
    List {
        /// The organization, by name or identifier.
        #[arg(long)]
        organization: Option<String>,
    },
    /// Adds or removes members, and changes settings.
    Set {
        /// The group, `egr_…`.
        group: String,
        /// A member to add, `nd_…`; repeat for more.
        #[arg(long = "add-member")]
        add: Vec<String>,
        /// A member to remove, `nd_…`; repeat for more.
        #[arg(long = "remove-member")]
        remove: Vec<String>,
        /// How fast it fails over, and which ports it gives.
        #[command(flatten)]
        settings: Settings,
    },
    /// Deletes a group no address pool names.
    Delete {
        /// The group, `egr_…`.
        group: String,
    },
}

fn span(flag: &str, s: &str) -> Result<buffa_types::google::protobuf::Duration, CliError> {
    let d = korpis_core::quantity::parse_duration(s)
        .map_err(|e| CliError::Local(format!("--{flag} {s:?}: a span such as 10s ({e})")))?;
    Ok(korpis_wire::duration(d))
}

/// `g` with what `s` sets.
fn apply(mut g: pb::EdgeGroup, s: &Settings) -> Result<pb::EdgeGroup, CliError> {
    if let Some(ports) = &s.ports {
        let bad = || CliError::Local(format!("--ports {ports:?}: a range such as 20000-40000"));
        let (first, last) = ports.split_once('-').ok_or_else(bad)?;
        g.first_port = first.trim().parse().map_err(|_| bad())?;
        g.last_port = last.trim().parse().map_err(|_| bad())?;
    }
    g.probe_port = s.probe_port.unwrap_or(g.probe_port);
    g.probe_misses = s.probe_misses.unwrap_or(g.probe_misses);
    if let Some(i) = &s.probe_interval {
        g.probe_interval = span("probe-interval", i)?.into();
    }
    if let Some(t) = &s.term {
        g.term = span("term", t)?.into();
    }
    Ok(g)
}

fn rows(list: &[pb::EdgeGroup]) -> String {
    let head = ["ID", "NAME", "MEMBERS", "PORTS", "DETECTS IN", "TERM", "MADE"];
    let mut all = vec![head.map(str::to_owned).to_vec()];
    let span = |d: Option<&buffa_types::google::protobuf::Duration>| {
        d.and_then(|d| korpis_wire::duration_from("", d).ok()).unwrap_or_default()
    };
    all.extend(list.iter().map(|g| {
        let detects = span(g.probe_interval.as_option()).saturating_mul(g.probe_misses);
        vec![
            g.id.clone(),
            g.name.clone(),
            g.members.join(","),
            format!("{}-{}", g.first_port, g.last_port),
            format!("{detects:?}"),
            format!("{:?}", span(g.term.as_option())),
            time(g.created_at.as_option()),
        ]
    }));
    table(&all)
}

async fn get(c: &Connection, group: &str) -> Result<pb::EdgeGroup, CliError> {
    let client = pb::EdgeGroupServiceClient::new(c.http(), c.config());
    let r = pb::GetEdgeGroupRequest { id: group.to_owned(), ..Default::default() };
    Ok(client.get_edge_group(r).await?.into_owned().group.into_option().unwrap_or_default())
}

/// A group's status, as reading it shows: each member's probe, and each address's holder.
/// Where `h`'s router routes it: `-` on a link the members share; the member it routes it to,
/// or nowhere, where that is its holder or it has none; and otherwise that it waits on the
/// router, and why where the router failed.
fn routed(h: &pb::EdgeHolding) -> String {
    if h.router.is_empty() {
        return "-".into();
    }
    if h.routed_to == h.node || h.node.is_empty() {
        let to = if h.routed_to.is_empty() { "nowhere" } else { &h.routed_to };
        return format!("{to} by {}", h.router);
    }
    if h.route_failure.is_empty() {
        format!("waiting on {}", h.router)
    } else {
        format!("waiting on {}: {}", h.router, h.route_failure)
    }
}

fn status(g: &pb::EdgeGroup) -> String {
    let mut out = String::new();
    if !g.ingress_addresses.is_empty() {
        out.push_str("\ningress at ");
        out.push_str(&g.ingress_addresses.join(", "));
        out.push('\n');
    }
    if !g.member_status.is_empty() {
        let mut all =
            vec![["MEMBER", "READY", "LAST PROBE", "WHY NOT"].map(str::to_owned).to_vec()];
        all.extend(g.member_status.iter().map(|m| {
            let ready = if m.ready { "yes" } else { "no" };
            vec![m.node.clone(), ready.into(), time(m.last_probe.as_option()), m.reason.clone()]
        }));
        out.push('\n');
        out.push_str(&table(&all));
    }
    if !g.holdings.is_empty() {
        let head = ["ADDRESS", "HOLDER", "EPOCH", "ANSWERS", "ROUTED"];
        let mut all = vec![head.map(str::to_owned).to_vec()];
        all.extend(g.holdings.iter().map(|h| {
            let answers = match (h.last_probe.as_option(), h.answering) {
                (None, _) => "not yet probed",
                (Some(_), true) => "yes",
                (Some(_), false) => "no",
            };
            let holder = if h.node.is_empty() { "none".to_owned() } else { h.node.clone() };
            vec![h.address.clone(), holder, h.epoch.to_string(), answers.into(), routed(h)]
        }));
        out.push('\n');
        out.push_str(&table(&all));
    }
    out
}

fn one(g: &pb::EdgeGroup) -> Answer {
    Answer { human: format!("{}{}", rows(std::slice::from_ref(g)), status(g)), json: json(g) }
}

/// Answers `korpis edge-group …`.
pub async fn edge_group(c: &Connection, action: &EdgeGroupAction) -> Result<Answer, CliError> {
    let client = pb::EdgeGroupServiceClient::new(c.http(), c.config());
    Ok(match action {
        EdgeGroupAction::Create { name, organization, members, settings } => {
            let asked = pb::EdgeGroup {
                organization: resolve::organization(c, organization.as_deref()).await?,
                name: name.clone(),
                members: members.clone(),
                ..Default::default()
            };
            let r = pb::CreateEdgeGroupRequest {
                mutation: mutation(None).into(),
                group: apply(asked, settings)?.into(),
                ..Default::default()
            };
            let made = client.create_edge_group(r).await?.into_owned().group;
            one(&made.into_option().unwrap_or_default())
        }
        EdgeGroupAction::Get { group } => one(&get(c, group).await?),
        EdgeGroupAction::List { organization } => {
            let r = pb::ListEdgeGroupsRequest {
                organization: resolve::organization(c, organization.as_deref()).await?,
                ..Default::default()
            };
            let list = client.list_edge_groups(r).await?.into_owned().groups;
            Answer { human: rows(&list), json: json(&list) }
        }
        EdgeGroupAction::Set { group, add, remove, settings } => {
            let mut g = get(c, group).await?;
            g.members.retain(|m| !remove.contains(m));
            g.members
                .extend(add.iter().filter(|m| !g.members.contains(m)).cloned().collect::<Vec<_>>());
            let r = pb::UpdateEdgeGroupRequest {
                mutation: mutation(None).into(),
                group: apply(g, settings)?.into(),
                ..Default::default()
            };
            let changed = client.update_edge_group(r).await?.into_owned().group;
            one(&changed.into_option().unwrap_or_default())
        }
        EdgeGroupAction::Delete { group } => {
            let r = pb::DeleteEdgeGroupRequest {
                mutation: mutation(None).into(),
                id: group.clone(),
                ..Default::default()
            };
            client.delete_edge_group(r).await?;
            Answer { human: format!("deleted {group}\n"), json: serde_json::json!({}) }
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_group_shows_its_members_ports_and_how_fast_it_fails_over() {
        let asked = Settings {
            ports: Some("20000-40000".into()),
            probe_interval: Some("500ms".into()),
            probe_misses: Some(4),
            term: Some("10s".into()),
            ..Settings::default()
        };
        let g = pb::EdgeGroup {
            id: "egr_1".into(),
            name: "edge".into(),
            members: vec!["nd_1".into(), "nd_2".into()],
            ..Default::default()
        };
        let human = rows(&[apply(g, &asked).unwrap()]);
        assert!(human.contains("nd_1,nd_2") && human.contains("20000-40000"), "{human}");
        assert!(human.contains("2s") && human.contains("10s"), "{human}");
        let bad = Settings { ports: Some("20000".into()), ..Settings::default() };
        assert!(apply(pb::EdgeGroup::default(), &bad).is_err());

        let probed = pb::EdgeGroup {
            member_status: vec![pb::EdgeMemberStatus {
                node: "nd_2".into(),
                reason: "it missed 3 probes in a row through its forwarding path".into(),
                ..Default::default()
            }],
            holdings: vec![pb::EdgeHolding {
                address: "198.51.100.2".into(),
                node: "nd_1".into(),
                epoch: 7,
                ..Default::default()
            }],
            ingress_addresses: vec!["198.51.100.1".into()],
            ..Default::default()
        };
        let shown = status(&probed);
        assert!(shown.contains("missed 3 probes") && shown.contains("not yet probed"), "{shown}");
        assert!(shown.contains("ingress at 198.51.100.1"), "{shown}");

        let holding = |routed_to: &str, route_failure: &str| pb::EdgeHolding {
            node: "nd_1".into(),
            router: "bgp".into(),
            routed_to: routed_to.into(),
            route_failure: route_failure.into(),
            ..Default::default()
        };
        assert_eq!(routed(&holding("nd_1", "")), "nd_1 by bgp");
        assert_eq!(
            routed(&holding("nd_2", "bgp.session_down")),
            "waiting on bgp: bgp.session_down"
        );
        assert_eq!(routed(&pb::EdgeHolding::default()), "-", "on a link the members share");
    }
}
