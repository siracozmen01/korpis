// SPDX-License-Identifier: AGPL-3.0-or-later

//! Names to identifiers, through what the caller may read. A name is looked up by listing what
//! the API shows the caller, so a name the caller cannot read resolves to nothing, exactly as the
//! API answers `NOT_FOUND` for it (§8 of docs/10-api.md); the CLI never learns more by resolving
//! than by asking.

use crate::connect::Connection;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;

/// Every organization the caller may read.
pub async fn organizations(c: &Connection) -> Result<Vec<pb::Organization>, CliError> {
    let client = pb::OrganizationServiceClient::new(c.http(), c.config());
    let request = pb::ListOrganizationsRequest::default();
    Ok(client.list_organizations(request).await?.into_owned().organizations)
}

/// The one of `ids` found, or why there is not exactly one.
fn exactly_one(what: &str, named: &str, ids: &[String]) -> Result<String, CliError> {
    match ids {
        [one] => Ok(one.clone()),
        [] => Err(CliError::Local(format!("no {what} named {named:?} that you can read"))),
        _ => Err(CliError::Local(format!("{named:?} names several {what}s; use an identifier"))),
    }
}

/// The organization `named`, by identifier or name; with none named, the one organization the
/// caller can read that has no parent among them.
pub async fn organization(c: &Connection, named: Option<&str>) -> Result<String, CliError> {
    if let Some(id) = named.filter(|n| n.starts_with("org_")) {
        return Ok(id.to_owned());
    }
    let orgs = organizations(c).await?;
    let ids: Vec<String> = if let Some(n) = named {
        orgs.iter().filter(|o| o.name == n).map(|o| o.id.clone()).collect()
    } else {
        let known: Vec<&str> = orgs.iter().map(|o| o.id.as_str()).collect();
        let top = |o: &&pb::Organization| o.parent.as_deref().is_none_or(|p| !known.contains(&p));
        orgs.iter().filter(top).map(|o| o.id.clone()).collect()
    };
    exactly_one("organization", named.unwrap_or("(the top)"), &ids)
}

/// Every project in organization `org`.
pub async fn projects(c: &Connection, org: &str) -> Result<Vec<pb::Project>, CliError> {
    let client = pb::ProjectServiceClient::new(c.http(), c.config());
    let request = pb::ListProjectsRequest { organization: org.to_owned(), ..Default::default() };
    Ok(client.list_projects(request).await?.into_owned().projects)
}

/// The project `named`: a `prj_` identifier, or a name unique among the projects the caller may
/// read.
pub async fn project(c: &Connection, named: &str) -> Result<String, CliError> {
    if named.starts_with("prj_") {
        return Ok(named.to_owned());
    }
    let mut ids = Vec::new();
    for o in organizations(c).await? {
        ids.extend(projects(c, &o.id).await?.into_iter().filter(|p| p.name == named).map(|p| p.id));
    }
    exactly_one("project", named, &ids)
}

/// Every workload in project `project`.
pub async fn workloads(c: &Connection, project: &str) -> Result<Vec<pb::WorkloadStatus>, CliError> {
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let request =
        pb::ListWorkloadsRequest { project: Some(project.to_owned()), ..Default::default() };
    Ok(client.list_workloads(request).await?.into_owned().workloads)
}

/// The workload `named`: a `wl_` identifier, `project/name`, or a name unique among the
/// workloads the caller may read.
pub async fn workload(c: &Connection, named: &str) -> Result<String, CliError> {
    if named.starts_with("wl_") {
        return Ok(named.to_owned());
    }
    let (in_project, name) = match named.split_once('/') {
        Some((p, n)) => (Some(project(c, p).await?), n),
        None => (None, named),
    };
    let mut candidates = Vec::new();
    match in_project {
        Some(p) => candidates.push(p),
        None => {
            for o in organizations(c).await? {
                candidates.extend(projects(c, &o.id).await?.into_iter().map(|p| p.id));
            }
        }
    }
    let mut ids = Vec::new();
    for p in candidates {
        let found = workloads(c, &p).await?.into_iter().filter_map(|s| s.workload.into_option());
        ids.extend(found.filter(|w| w.name == name).map(|w| w.id));
    }
    exactly_one("workload", named, &ids)
}

/// The node `named`: an `nd_` identifier, or a name unique among the nodes the caller may read.
pub async fn node(c: &Connection, named: &str) -> Result<String, CliError> {
    if named.starts_with("nd_") {
        return Ok(named.to_owned());
    }
    let client = pb::NodeServiceClient::new(c.http(), c.config());
    let mut ids = Vec::new();
    for o in organizations(c).await? {
        let request = pb::ListNodesRequest { organization: o.id, ..Default::default() };
        let nodes = client.list_nodes(request).await?.into_owned().nodes;
        ids.extend(nodes.into_iter().filter(|n| n.name == named).map(|n| n.id));
    }
    exactly_one("node", named, &ids)
}
