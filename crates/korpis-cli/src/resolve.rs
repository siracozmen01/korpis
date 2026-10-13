// SPDX-License-Identifier: AGPL-3.0-or-later

//! Names to identifiers, through what the caller may read. A name is looked up by listing what
//! the API shows the caller, so a name the caller cannot read resolves to nothing, exactly as the
//! API answers `NOT_FOUND` for it (§8 of docs/10-api.md); the CLI never learns more by resolving
//! than by asking.
//!
//! Reading starts from two places: every organization the caller may read, and the objects its
//! own grants are anchored at. The second is what lets a token scoped to one project name that
//! project, though it can read no organization to list it from (§2 of docs/13-surface-cli.md).

use crate::connect::Connection;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::scope::Kind as K;

/// Every organization the caller may read.
pub async fn organizations(c: &Connection) -> Result<Vec<pb::Organization>, CliError> {
    let client = pb::OrganizationServiceClient::new(c.http(), c.config());
    let request = pb::ListOrganizationsRequest::default();
    Ok(client.list_organizations(request).await?.into_owned().organizations)
}

/// `Some` of what was read, `None` where the caller may not read it.
pub fn readable<T>(r: Result<T, CliError>) -> Result<Option<T>, CliError> {
    match r {
        Ok(t) => Ok(Some(t)),
        Err(CliError::Api {
            code: pb::ErrorCode::ERROR_CODE_DENIED | pb::ErrorCode::ERROR_CODE_NOT_FOUND,
            ..
        }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The objects the caller's own grants are anchored at, by kind.
#[derive(Debug, Default)]
struct Anchors {
    organizations: Vec<String>,
    projects: Vec<String>,
    workloads: Vec<String>,
    nodes: Vec<String>,
}

fn anchor(scope: Option<&pb::Scope>, into: &mut Anchors) {
    let list = match scope.and_then(|s| s.kind.as_ref()) {
        Some(K::Organization(id)) => (&mut into.organizations, id),
        Some(K::Project(id)) => (&mut into.projects, id),
        Some(K::Workload(id)) => (&mut into.workloads, id),
        Some(K::Node(id)) => (&mut into.nodes, id),
        Some(K::Selector(s)) => return anchor(s.within.as_option(), into),
        _ => return,
    };
    if !list.0.contains(list.1) {
        list.0.push(list.1.clone());
    }
}

async fn anchors(c: &Connection) -> Result<Anchors, CliError> {
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let grants = client.list_grants(pb::ListGrantsRequest::default()).await?.into_owned().grants;
    let mut out = Anchors::default();
    let live =
        grants.iter().filter(|g| g.state.as_known() == Some(pb::GrantState::GRANT_STATE_ISSUED));
    for g in live {
        anchor(g.scope.as_option(), &mut out);
    }
    Ok(out)
}

/// The one of `ids` found, or why there is not exactly one.
fn exactly_one(what: &str, named: &str, ids: &[String]) -> Result<String, CliError> {
    match ids {
        [one] => Ok(one.clone()),
        [] => Err(CliError::Local(format!("no {what} named {named:?} that you can read"))),
        _ => Err(CliError::Local(format!("{named:?} names several {what}s; use an identifier"))),
    }
}

/// The organizations reading starts from: those the caller may read, and those its grants are
/// anchored at.
pub async fn organization_ids(c: &Connection) -> Result<Vec<String>, CliError> {
    let mut ids: Vec<String> = organizations(c).await?.into_iter().map(|o| o.id).collect();
    for o in anchors(c).await?.organizations {
        if !ids.contains(&o) {
            ids.push(o);
        }
    }
    Ok(ids)
}

/// The organization `named`, by identifier or name; with none named, the one organization the
/// caller can read that has no parent among them, or else the one its grants are anchored at.
pub async fn organization(c: &Connection, named: Option<&str>) -> Result<String, CliError> {
    if let Some(id) = named.filter(|n| n.starts_with("org_")) {
        return Ok(id.to_owned());
    }
    let orgs = organizations(c).await?;
    let ids: Vec<String> = if let Some(n) = named {
        orgs.iter().filter(|o| o.name == n).map(|o| o.id.clone()).collect()
    } else if orgs.is_empty() {
        anchors(c).await?.organizations
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

/// Every project the caller may read: listed from its organizations, and those its grants are
/// anchored at.
pub async fn readable_projects(c: &Connection) -> Result<Vec<pb::Project>, CliError> {
    let mut found = Vec::new();
    for o in organization_ids(c).await? {
        found.extend(readable(projects(c, &o).await)?.unwrap_or_default());
    }
    let client = pb::ProjectServiceClient::new(c.http(), c.config());
    for id in anchors(c).await?.projects {
        if found.iter().any(|p: &pb::Project| p.id == id) {
            continue;
        }
        let request = pb::GetProjectRequest { id, ..Default::default() };
        let got = readable(client.get_project(request).await.map_err(CliError::from))?;
        found.extend(got.and_then(|r| r.into_owned().project.into_option()));
    }
    Ok(found)
}

/// The project `named`: a `prj_` identifier, or a name unique among the projects the caller may
/// read.
pub async fn project(c: &Connection, named: &str) -> Result<String, CliError> {
    if named.starts_with("prj_") {
        return Ok(named.to_owned());
    }
    let projects = readable_projects(c).await?;
    let ids: Vec<String> = projects.into_iter().filter(|p| p.name == named).map(|p| p.id).collect();
    exactly_one("project", named, &ids)
}

/// Every workload in project `project`.
pub async fn workloads(c: &Connection, project: &str) -> Result<Vec<pb::WorkloadStatus>, CliError> {
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let request =
        pb::ListWorkloadsRequest { project: Some(project.to_owned()), ..Default::default() };
    Ok(client.list_workloads(request).await?.into_owned().workloads)
}

/// The workloads the caller's grants are anchored at.
async fn anchored_workloads(c: &Connection) -> Result<Vec<pb::Workload>, CliError> {
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let mut found = Vec::new();
    for id in anchors(c).await?.workloads {
        let request = pb::GetWorkloadRequest { id, ..Default::default() };
        let got = readable(client.get_workload(request).await.map_err(CliError::from))?;
        let status = got.and_then(|r| r.into_owned().status.into_option());
        found.extend(status.and_then(|s| s.workload.into_option()));
    }
    Ok(found)
}

/// Every workload the caller may read in `project`, or in every project it may read, with the
/// name of the project each is in.
pub async fn readable_workloads(
    c: &Connection,
    project: Option<&str>,
) -> Result<Vec<(String, pb::Workload)>, CliError> {
    let projects: Vec<pb::Project> = readable_projects(c).await?;
    let name_of = |id: &str| projects.iter().find(|p| p.id == id).map(|p| p.name.clone());
    let candidates: Vec<String> = match project {
        Some(p) => vec![p.to_owned()],
        None => projects.iter().map(|p| p.id.clone()).collect(),
    };
    let mut found: Vec<pb::Workload> = Vec::new();
    for p in candidates {
        let listed = readable(workloads(c, &p).await)?.unwrap_or_default();
        found.extend(listed.into_iter().filter_map(|s| s.workload.into_option()));
    }
    for w in anchored_workloads(c).await? {
        let in_scope = project.is_none_or(|p| p == w.project);
        if in_scope && !found.iter().any(|f| f.id == w.id) {
            found.push(w);
        }
    }
    Ok(found
        .into_iter()
        .map(|w| (name_of(&w.project).unwrap_or_else(|| w.project.clone()), w))
        .collect())
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
    let found = readable_workloads(c, in_project.as_deref()).await?;
    let ids: Vec<String> =
        found.into_iter().filter(|(_, w)| w.name == name).map(|(_, w)| w.id).collect();
    exactly_one("workload", named, &ids)
}

/// Every node the caller may read.
pub async fn readable_nodes(c: &Connection) -> Result<Vec<pb::Node>, CliError> {
    let client = pb::NodeServiceClient::new(c.http(), c.config());
    let mut found: Vec<pb::Node> = Vec::new();
    for o in organization_ids(c).await? {
        let request = pb::ListNodesRequest { organization: o, ..Default::default() };
        let listed = readable(client.list_nodes(request).await.map_err(CliError::from))?;
        found.extend(listed.map(|r| r.into_owned().nodes).unwrap_or_default());
    }
    for id in anchors(c).await?.nodes {
        if !found.iter().any(|n| n.id == id) {
            let request = pb::GetNodeRequest { id, ..Default::default() };
            let got = readable(client.get_node(request).await.map_err(CliError::from))?;
            found.extend(got.and_then(|r| r.into_owned().node.into_option()));
        }
    }
    Ok(found)
}

/// The node `named`: an `nd_` identifier, or a name unique among the nodes the caller may read.
pub async fn node(c: &Connection, named: &str) -> Result<String, CliError> {
    if named.starts_with("nd_") {
        return Ok(named.to_owned());
    }
    let found = readable_nodes(c).await?;
    let ids: Vec<String> = found.into_iter().filter(|n| n.name == named).map(|n| n.id).collect();
    exactly_one("node", named, &ids)
}
