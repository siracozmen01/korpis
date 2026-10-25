// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dependencies resolved (§5 of docs/07-networking.md): the control plane's half, which endpoint
//! each target means. The node's half, where that endpoint's workload is, is the node's, since
//! its agent alone allocated the workload's address; for a target on another node of the mesh
//! the control plane carries the address that node observed (§9.1 there).

use super::endpoint::Endpoint;
use super::spec::IntentBody;
use super::spec::dependencies::{Dependency, DependencyTarget, Injection};
use super::spec::endpoints::{EndpointSpec, Exposure, Protocol};
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use uuid::Uuid;

/// One endpoint a dependency reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetEndpoint {
    /// The workload it is an endpoint of.
    pub workload: Uuid,
    /// The endpoint's name.
    pub endpoint: String,
    /// Its port inside the workload.
    pub container_port: u16,
    /// What it speaks.
    pub protocol: Protocol,
    /// Where its workload is on another node of the mesh, as that node observed it: its
    /// address there, which the dependent's node holds no other way (§9.1 of
    /// docs/07-networking.md); none where it is on the dependent's node, or not observed yet.
    pub address: Option<Ipv4Addr>,
    /// Whether it is ready, as that other node observed it; the dependent's node judges a
    /// target on itself by itself.
    pub ready: bool,
}

/// A dependent on another node of the mesh, which the node of the workload it depends on
/// admits from the mesh to that one endpoint (§9.1 of docs/07-networking.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayDependent {
    /// The dependent.
    pub workload: Uuid,
    /// Its address on its node, as that node observed it.
    pub address: Ipv4Addr,
    /// The address of the workload it depends on, on that workload's node.
    pub target_address: Ipv4Addr,
    /// The endpoint's port inside that workload.
    pub container_port: u16,
    /// What it speaks.
    pub protocol: Protocol,
}

/// A dependency as its node is told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDependency {
    /// Its name.
    pub name: String,
    /// How it is injected.
    pub inject: Injection,
    /// Whether the dependent starts only once it is ready.
    pub required: bool,
    /// What it reaches now, in the order of the targets' workload names.
    pub targets: Vec<TargetEndpoint>,
    /// The workloads it names that it does not reach, and why: a selector's match, or a named
    /// workload whose endpoints changed since it was declared (§5 of docs/07-networking.md).
    /// The node is not told them; the dependent's status shows them.
    pub left_out: Vec<(Uuid, NotATarget)>,
}

/// How a dependent reaches one of its targets, as its status shows it (§5 and §9.1 of
/// docs/07-networking.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Both run on one node.
    SameNode,
    /// They run on two nodes that reach each other through the mesh.
    Mesh,
    /// They run on two nodes that do not reach each other now.
    Unreached,
    /// The target is not placed.
    TargetUnplaced,
    /// The dependent is not placed, so nothing reaches anything yet.
    DependentUnplaced,
}

/// One target of a dependency as the dependent's status shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetStatus {
    /// The workload it is an endpoint of.
    pub workload: Uuid,
    /// The endpoint's name.
    pub endpoint: String,
    /// How the dependent reaches it.
    pub reach: Reach,
}

/// A dependency as its dependent's status shows it: what it reaches, how, and what it names
/// and does not reach, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyStatus {
    /// Its name.
    pub name: String,
    /// Whether the dependent starts only once it is ready.
    pub required: bool,
    /// Its targets.
    pub targets: Vec<TargetStatus>,
    /// The workloads it names and does not reach, and why.
    pub left_out: Vec<(Uuid, NotATarget)>,
}

/// Why a workload is not a dependency's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotATarget {
    /// It has no endpoint anything can reach.
    NoEndpoint,
    /// It has several, and nothing says which.
    SeveralEndpoints,
}

impl NotATarget {
    /// For a person.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoEndpoint => "it has no endpoint whose exposure is not none",
            Self::SeveralEndpoints => "it has several endpoints: name one by its identifier",
        }
    }
}

/// The one endpoint of `body` a dependency on its workload reaches: the one whose exposure is
/// not `none`.
pub fn reachable(body: &IntentBody) -> Result<&EndpointSpec, NotATarget> {
    let mut reached = body.endpoints.iter().filter(|e| e.exposure != Exposure::None);
    match (reached.next(), reached.next()) {
        (Some(e), None) => Ok(e),
        (None, _) => Err(NotATarget::NoEndpoint),
        (Some(_), Some(_)) => Err(NotATarget::SeveralEndpoints),
    }
}

/// A workload of the dependent's project, as resolution reads it.
#[derive(Debug, Clone, Copy)]
pub struct ProjectWorkload<'a> {
    /// The workload.
    pub id: Uuid,
    /// Its name, which orders a selector's matches.
    pub name: &'a str,
    /// Its labels, which selectors match.
    pub labels: &'a BTreeMap<String, String>,
    /// The intent it is to run, if it has one.
    pub body: Option<&'a IntentBody>,
}

fn of_workload(w: &ProjectWorkload<'_>) -> Option<Result<TargetEndpoint, (Uuid, NotATarget)>> {
    // A workload with no intent yet is nothing to reach and nothing to report.
    Some(reachable(w.body?).map_err(|why| (w.id, why)).map(|e| TargetEndpoint {
        workload: w.id,
        endpoint: e.name.clone(),
        container_port: e.container_port,
        protocol: e.protocol,
        address: None,
        ready: false,
    }))
}

/// `d` resolved among `project`, the dependent's project's workloads other than itself, and
/// `endpoint`, the placed endpoint an endpoint target names if it is one of the project's.
#[must_use]
pub fn resolve(
    d: &Dependency,
    project: &[ProjectWorkload<'_>],
    endpoint: Option<&Endpoint>,
) -> ResolvedDependency {
    let named: Vec<Result<TargetEndpoint, (Uuid, NotATarget)>> = match &d.target {
        DependencyTarget::Workload(id) => {
            project.iter().filter(|w| w.id == *id).filter_map(of_workload).collect()
        }
        DependencyTarget::Endpoint(id) => endpoint
            .filter(|e| e.id == *id && project.iter().any(|w| w.id == e.workload))
            .map(|e| {
                Ok(TargetEndpoint {
                    workload: e.workload,
                    endpoint: e.name.clone(),
                    container_port: e.container_port,
                    protocol: e.protocol,
                    address: None,
                    ready: false,
                })
            })
            .into_iter()
            .collect(),
        DependencyTarget::Selector(selector) => {
            let mut matched: Vec<_> =
                project.iter().filter(|w| selector.matches(w.labels)).collect();
            matched.sort_by_key(|w| w.name);
            matched.into_iter().filter_map(of_workload).collect()
        }
    };
    let (targets, left_out): (Vec<_>, Vec<_>) = named.into_iter().partition(Result::is_ok);
    let targets = targets.into_iter().filter_map(Result::ok).collect();
    let left_out = left_out.into_iter().filter_map(Result::err).collect();
    ResolvedDependency {
        name: d.name.clone(),
        inject: d.inject,
        required: d.required,
        targets,
        left_out,
    }
}

#[cfg(test)]
mod tests;
