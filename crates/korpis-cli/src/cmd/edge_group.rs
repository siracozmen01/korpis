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

fn one(g: &pb::EdgeGroup) -> Answer {
    Answer { human: rows(std::slice::from_ref(g)), json: json(g) }
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
    }
}
