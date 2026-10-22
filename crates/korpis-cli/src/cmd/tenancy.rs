// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis organization …` and `korpis project …` (§2 of docs/08-identity.md): the tenancy tree,
//! grown from where the caller's grants reach. An organization is made under one the caller may
//! make organizations in, a reseller's beneath its own and never beside it, and a project in an
//! organization the caller may make projects in. Listing them is `korpis get`. An organization
//! is suspended and resumed from above, for non-payment, and what its tenant keeps while it is
//! suspended is said where it is shown.

use super::get::subject;
use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use std::fmt::Write as _;

/// `korpis organization …`.
#[derive(Debug, Clone, Subcommand)]
pub enum OrganizationAction {
    /// Makes an organization beneath another: a customer of a host, or of a reseller.
    Create {
        /// Its name, unique among the organizations beside it.
        name: String,
        /// The organization it is made beneath, by name or identifier; the top one you can read
        /// by default.
        #[arg(long)]
        parent: Option<String>,
    },
    /// One organization: its name, where it is in the tree, and its state.
    Show {
        /// The organization, by name or identifier; the top one you can read by default.
        organization: Option<String>,
    },
    /// Suspends an organization, from above: its grants, and those of every organization beneath
    /// it, stop authorizing all but the reads its tenant keeps, until it is resumed.
    Suspend {
        /// The organization, by name or identifier.
        organization: String,
        /// Why, as its tenant will be shown it.
        #[arg(long)]
        reason: String,
        /// Holds every workload beneath it stopped as well, whatever each declares, until it is
        /// resumed; this takes the authority to stop them.
        #[arg(long)]
        stop: bool,
    },
    /// Resumes a suspended organization: every grant and workload its suspension held is as it
    /// was; resuming one that held its workloads stopped takes the authority to start them.
    Resume {
        /// The organization, by name or identifier.
        organization: String,
    },
    /// The delegation ceiling at an organization: the longest a grant beneath a set subject
    /// lasts there (§5.1 of docs/08-identity.md). With `--set` or `--remove`, changes its own,
    /// under `org.write` there; below the root it may only lower what is in force above it.
    Ceiling {
        /// The organization, by name or identifier; the top one you can read by default.
        organization: Option<String>,
        /// Sets its own: a number then h or d, an hour at least.
        #[arg(long, conflicts_with = "remove")]
        set: Option<String>,
        /// Removes its own, leaving the one above it in force.
        #[arg(long)]
        remove: bool,
    },
    /// The placement policy at an organization (§2.2 of docs/05-scheduling.md): its strategy
    /// and weights score the nodes its subtree owns, and its spread and affinity place its
    /// subtree's workloads, wherever no organization nearer sets one. With `-f` or `--remove`,
    /// changes its own, under `org.write` there.
    Placement {
        /// The organization, by name or identifier; the top one you can read by default.
        organization: Option<String>,
        /// Sets its own from a file, JSON or YAML, as `--json` prints a policy.
        #[arg(short = 'f', long = "file", conflicts_with = "remove")]
        file: Option<std::path::PathBuf>,
        /// Removes its own, leaving the one above it, or the installation's, in force.
        #[arg(long)]
        remove: bool,
    },
}

/// `korpis project …`.
#[derive(Debug, Clone, Subcommand)]
pub enum ProjectAction {
    /// Makes a project in an organization.
    Create {
        /// Its name, unique in its organization.
        name: String,
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
}

/// What a suspended organization's tenant still does, and what it does not (§2.1 of
/// docs/08-identity.md).
const KEPT: &str = "its grants, organizations, projects, plans, nodes, and the status of its \
                    workloads stay readable; every change, a console, and a file are refused";

fn shown(o: &pb::Organization) -> String {
    let parent = o.parent.as_deref().unwrap_or("none, a root");
    let state = short(o.status, "ORGANIZATION_STATUS_");
    let mut out = format!("{} ({})\nparent:  {parent}\nstate:   {state}\n", o.name, o.id);
    if let Some(s) = o.suspension.as_option() {
        let stops = s.on_suspend.as_known() == Some(pb::OnSuspend::ON_SUSPEND_STOP);
        let workloads = if stops { "held stopped" } else { "kept running" };
        let (at, by) = (time(s.at.as_option()), subject(s.by.as_option()));
        let _ = write!(out, "since:   {at} by {by}\nreason:  {}\n", s.reason);
        let _ = write!(out, "workloads: {workloads}\nwhile suspended: {KEPT}\n");
    }
    out
}

/// What suspending `organization` asks.
fn suspension(organization: String, reason: &str, stop: bool) -> pb::SuspendOrganizationRequest {
    let on_suspend =
        if stop { pb::OnSuspend::ON_SUSPEND_STOP } else { pb::OnSuspend::ON_SUSPEND_KEEP_RUNNING };
    pb::SuspendOrganizationRequest {
        mutation: mutation(None).into(),
        organization,
        reason: reason.to_owned(),
        on_suspend: on_suspend.into(),
        ..Default::default()
    }
}

/// Answers `korpis organization …`.
pub async fn organization(c: &Connection, a: &OrganizationAction) -> Result<Answer, CliError> {
    let client = pb::OrganizationServiceClient::new(c.http(), c.config());
    let o = match a {
        OrganizationAction::Create { name, parent } => {
            let request = pb::CreateOrganizationRequest {
                mutation: mutation(None).into(),
                name: name.clone(),
                parent: Some(resolve::organization(c, parent.as_deref()).await?),
                ..Default::default()
            };
            client.create_organization(request).await?.into_owned().organization
        }
        OrganizationAction::Show { organization } => {
            let id = resolve::organization(c, organization.as_deref()).await?;
            let request = pb::GetOrganizationRequest { id, ..Default::default() };
            client.get_organization(request).await?.into_owned().organization
        }
        OrganizationAction::Suspend { organization, reason, stop } => {
            let id = resolve::organization(c, Some(organization)).await?;
            let request = suspension(id, reason, *stop);
            client.suspend_organization(request).await?.into_owned().organization
        }
        OrganizationAction::Resume { organization } => {
            let request = pb::ResumeOrganizationRequest {
                mutation: mutation(None).into(),
                organization: resolve::organization(c, Some(organization)).await?,
                ..Default::default()
            };
            client.resume_organization(request).await?.into_owned().organization
        }
        OrganizationAction::Ceiling { organization, set, remove } => {
            let asked = (set.as_deref(), *remove);
            return super::ceiling::ceiling(c, organization.as_deref(), asked).await;
        }
        OrganizationAction::Placement { organization, file, remove } => {
            let asked = (file.as_deref(), *remove);
            return super::placement::placement(c, organization.as_deref(), asked).await;
        }
    };
    let o = o.into_option().unwrap_or_default();
    Ok(Answer { human: shown(&o), json: json(&o) })
}

/// Answers `korpis project …`.
pub async fn project(c: &Connection, a: &ProjectAction) -> Result<Answer, CliError> {
    let ProjectAction::Create { name, organization } = a;
    let request = pb::CreateProjectRequest {
        mutation: mutation(None).into(),
        organization: resolve::organization(c, organization.as_deref()).await?,
        name: name.clone(),
        ..Default::default()
    };
    let client = pb::ProjectServiceClient::new(c.http(), c.config());
    let p = client.create_project(request).await?.into_owned().project;
    let p = p.into_option().unwrap_or_default();
    let human = format!("project {} ({}) in {}\n", p.name, p.id, p.organization);
    Ok(Answer { human, json: json(&p) })
}
