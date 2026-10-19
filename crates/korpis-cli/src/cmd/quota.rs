// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis quota show|set` (§5.1 of docs/05-scheduling.md): an organization's quota set, or a
//! project's, what the workloads each entry counts hold, and, for an organization, what it has
//! given below of each entry's shape: guaranteed beside what it was guaranteed, and sold beside
//! what it may sell, its own limit times the overcommit it was permitted, and each organization
//! beneath it with what it was given and holds. A set is set from above, from a file holding
//! what `quota show --json` prints of it.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::table;
use clap::Subcommand;
use korpis_core::tenancy::quota::{Bounds, Dimension};
use korpis_proto::korpis::v1 as pb;
use korpis_wire::quota::bounds_from_pb;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// `korpis quota …`.
#[derive(Debug, Clone, Subcommand)]
pub enum QuotaAction {
    /// An organization's quota set, or a project's, with what its workloads hold and what it
    /// has given below.
    Show {
        /// The organization, by name or identifier; the top one you can read by default.
        organization: Option<String>,
        /// One of its projects instead, by name or identifier.
        #[arg(long)]
        project: Option<String>,
    },
    /// Replaces an organization's quota set, or a project's, from a file: JSON or YAML, as
    /// `quota show --json` prints the set. Takes `org.quota.write` over the organization above
    /// it, or over the project's organization.
    Set {
        /// The organization, by name or identifier.
        organization: String,
        /// One of its projects instead, by name or identifier.
        #[arg(long)]
        project: Option<String>,
        /// The set.
        #[arg(short = 'f', long = "file")]
        file: PathBuf,
    },
}

/// A selector as a person reads it: `k=v,…`.
fn selector(s: Option<&pb::LabelSelector>) -> Option<String> {
    let s = s?;
    let mut parts: Vec<String> = s.match_labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
    parts.extend(
        s.match_expressions.iter().map(|e| format!("{} {:?} {:?}", e.key, e.operator, e.values)),
    );
    Some(parts.join(","))
}

/// What an entry counts, as a person reads it.
fn shape(e: &pb::QuotaEntry) -> String {
    let workloads = selector(e.selector.as_option()).unwrap_or_else(|| "all".into());
    match selector(e.nodes.as_option()) {
        Some(nodes) => format!("{workloads} on {nodes}"),
        None => workloads,
    }
}

fn same_shape(a: &pb::QuotaEntry, b: &pb::QuotaEntry) -> bool {
    a.selector == b.selector && a.nodes == b.nodes
}

fn cell(n: Option<u64>) -> String {
    n.map_or_else(|| "-".into(), |n| n.to_string())
}

/// What `children` were given of `e`'s shape, summed: their reservations and their limits.
fn given(e: &pb::QuotaEntry, children: &[pb::Organization]) -> (Bounds, Bounds) {
    let (mut reserved, mut sold) = (Bounds::new(), Bounds::new());
    let theirs = children
        .iter()
        .filter_map(|c| c.quota.as_option())
        .flat_map(|q| q.entries.iter().filter(|c| same_shape(c, e)));
    for c in theirs {
        for (bounds, into) in [
            (bounds_from_pb(c.reservation_bounds.as_option()), &mut reserved),
            (bounds_from_pb(c.limit_bounds.as_option()), &mut sold),
        ] {
            for (d, n) in bounds {
                let slot = into.entry(d).or_insert(0);
                *slot = slot.saturating_add(n);
            }
        }
    }
    (reserved, sold)
}

/// Each dimension `e` bounds, with what it guarantees, what is reserved of it, what it sells,
/// and what is limited of it.
fn bounded(e: &pb::QuotaEntry) -> Vec<(Dimension, [String; 4])> {
    let (reserved, limits) = (
        bounds_from_pb(e.reservation_bounds.as_option()),
        bounds_from_pb(e.limit_bounds.as_option()),
    );
    let held = e.held.as_option();
    let held_reserved = bounds_from_pb(held.and_then(|h| h.reservations.as_option()));
    let held_limits = bounds_from_pb(held.and_then(|h| h.limits.as_option()));
    let unbounded = held.map(|h| h.unbounded.clone()).unwrap_or_default();
    let dimensions: BTreeSet<_> = reserved.keys().chain(limits.keys()).copied().collect();
    let held_limit = |d: Dimension| {
        if unbounded.iter().any(|u| u == d.as_str()) {
            "unbounded".to_owned()
        } else {
            cell(held_limits.get(&d).copied().or(Some(0)))
        }
    };
    dimensions
        .into_iter()
        .map(|d| {
            let cells = [
                cell(reserved.get(&d).copied()),
                cell(held_reserved.get(&d).copied().or(Some(0))),
                cell(limits.get(&d).copied()),
                held_limit(d),
            ];
            (d, cells)
        })
        .collect()
}

/// The rows of `set`, one per entry and dimension it bounds.
fn rows(set: &pb::QuotaSet, children: &[pb::Organization]) -> String {
    let header = [
        "SHAPE",
        "DIMENSION",
        "GUARANTEED",
        "HELD",
        "GIVEN BELOW",
        "SOLD",
        "HELD",
        "SOLD BELOW",
        "MAY SELL",
    ];
    let mut all = vec![header.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>()];
    for e in &set.entries {
        let (given_reserved, given_sold) = given(e, children);
        let limits = bounds_from_pb(e.limit_bounds.as_option());
        for (d, [guaranteed, reserved, sold, limited]) in bounded(e) {
            let percent =
                u64::from(set.child_overcommit_percent.get(d.as_str()).copied().unwrap_or(100));
            all.push(vec![
                shape(e),
                d.as_str().to_owned(),
                guaranteed,
                reserved,
                cell(given_reserved.get(&d).copied().or(Some(0))),
                sold,
                limited,
                cell(given_sold.get(&d).copied().or(Some(0))),
                cell(limits.get(&d).map(|l| l.saturating_mul(percent) / 100)),
            ]);
        }
    }
    table(&all)
}

/// What each of `children` was given and holds, one row per entry and dimension it bounds.
fn below_rows(children: &[pb::Organization]) -> String {
    let header = ["ORGANIZATION", "SHAPE", "DIMENSION", "GUARANTEED", "HELD", "SOLD", "HELD"];
    let mut all = vec![header.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>()];
    let entries = children.iter().flat_map(|c| {
        c.quota.as_option().into_iter().flat_map(|q| &q.entries).map(move |e| (c, e))
    });
    for (child, e) in entries {
        for (d, [guaranteed, reserved, sold, limited]) in bounded(e) {
            let at = [child.name.clone(), shape(e), d.as_str().to_owned()];
            all.push(at.into_iter().chain([guaranteed, reserved, sold, limited]).collect());
        }
    }
    if all.len() == 1 {
        return "nothing below holds a quota\n".into();
    }
    table(&all)
}

/// Answers `korpis quota show` for an organization.
async fn show_organization(c: &Connection, named: Option<&str>) -> Result<Answer, CliError> {
    let client = pb::OrganizationServiceClient::new(c.http(), c.config());
    let get = |id: String| {
        let client = &client;
        async move {
            let request = pb::GetOrganizationRequest { id, ..Default::default() };
            let got = client.get_organization(request).await?.into_owned().organization;
            Ok::<_, CliError>(got.into_option().unwrap_or_default())
        }
    };
    let org = get(resolve::organization(c, named).await?).await?;
    let mut children = Vec::new();
    for child in resolve::beneath(c, &org.id).await? {
        children.push(get(child.id).await?);
    }
    let set = org.quota.as_option().cloned().unwrap_or_default();
    let human = format!(
        "quota of {} ({})\n{}\nbelow it\n{}",
        org.name,
        org.id,
        rows(&set, &children),
        below_rows(&children)
    );
    let below: Vec<_> = children
        .iter()
        .map(|c| serde_json::json!({ "organization": c.id, "name": c.name, "quota": json(&c.quota) }))
        .collect();
    Ok(Answer { json: serde_json::json!({ "quota": json(&set), "children": below }), human })
}

/// Answers `korpis quota …`.
pub async fn quota(c: &Connection, a: &QuotaAction) -> Result<Answer, CliError> {
    match a {
        QuotaAction::Show { organization, project: None } => {
            show_organization(c, organization.as_deref()).await
        }
        QuotaAction::Show { project: Some(p), .. } => {
            let id = resolve::project(c, p).await?;
            let client = pb::ProjectServiceClient::new(c.http(), c.config());
            let request = pb::GetProjectRequest { id, ..Default::default() };
            let project = client.get_project(request).await?.into_owned().project;
            let project = project.into_option().unwrap_or_default();
            let set = project.quota.as_option().cloned().unwrap_or_default();
            let human =
                format!("quota of project {} ({})\n{}", project.name, project.id, rows(&set, &[]));
            Ok(Answer { json: serde_json::json!({ "quota": json(&set) }), human })
        }
        QuotaAction::Set { organization, project, file } => {
            let at = file.display();
            let text =
                std::fs::read_to_string(file).map_err(|e| CliError::Local(format!("{at}: {e}")))?;
            let quota: pb::QuotaSet =
                serde_saphyr::from_str(&text).map_err(|e| CliError::Local(format!("{at}: {e}")))?;
            let project = match project {
                Some(p) => Some(resolve::project(c, p).await?),
                None => None,
            };
            let request = pb::SetQuotaRequest {
                mutation: mutation(None).into(),
                organization: resolve::organization(c, Some(organization)).await?,
                quota: quota.into(),
                project,
                ..Default::default()
            };
            let client = pb::OrganizationServiceClient::new(c.http(), c.config());
            let set = client.set_quota(request).await?.into_owned();
            let owner = set.project.as_option().map_or_else(
                || set.organization.as_option().map(|o| o.name.clone()).unwrap_or_default(),
                |p| format!("project {}", p.name),
            );
            Ok(Answer { human: format!("quota of {owner} set\n"), json: json(&set) })
        }
    }
}
