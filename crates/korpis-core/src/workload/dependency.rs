// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dependencies resolved (§5 of docs/07-networking.md): the control plane's half, which endpoint
//! each target means. The node's half, where that endpoint's workload is, is the node's, since
//! its agent alone allocated the workload's address.

use super::endpoint::Endpoint;
use super::spec::IntentBody;
use super::spec::dependencies::{Dependency, DependencyTarget, Injection};
use super::spec::endpoints::{EndpointSpec, Exposure, Protocol};
use std::collections::BTreeMap;
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

fn of_workload(w: &ProjectWorkload<'_>) -> Option<TargetEndpoint> {
    let e = reachable(w.body?).ok()?;
    Some(TargetEndpoint {
        workload: w.id,
        endpoint: e.name.clone(),
        container_port: e.container_port,
        protocol: e.protocol,
    })
}

/// `d` resolved among `project`, the dependent's project's workloads other than itself, and
/// `endpoint`, the placed endpoint an endpoint target names if it is one of the project's.
#[must_use]
pub fn resolve(
    d: &Dependency,
    project: &[ProjectWorkload<'_>],
    endpoint: Option<&Endpoint>,
) -> ResolvedDependency {
    let targets = match &d.target {
        DependencyTarget::Workload(id) => {
            project.iter().filter(|w| w.id == *id).filter_map(of_workload).collect()
        }
        DependencyTarget::Endpoint(id) => endpoint
            .filter(|e| e.id == *id && project.iter().any(|w| w.id == e.workload))
            .map(|e| TargetEndpoint {
                workload: e.workload,
                endpoint: e.name.clone(),
                container_port: e.container_port,
                protocol: e.protocol,
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
    ResolvedDependency { name: d.name.clone(), inject: d.inject, required: d.required, targets }
}

#[cfg(test)]
mod tests;
