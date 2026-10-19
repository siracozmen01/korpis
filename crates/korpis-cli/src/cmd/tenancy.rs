// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis organization …` and `korpis project …` (§2 of docs/08-identity.md): the tenancy tree,
//! grown from where the caller's grants reach. An organization is made under one the caller may
//! make organizations in, a reseller's beneath its own and never beside it, and a project in an
//! organization the caller may make projects in. Listing them is `korpis get`.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

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

fn shown(o: &pb::Organization) -> String {
    let parent = o.parent.as_deref().unwrap_or("none, a root");
    let state = crate::show::short(o.status, "ORGANIZATION_STATUS_").to_lowercase();
    format!("{} ({})\nparent:  {parent}\nstate:   {state}\n", o.name, o.id)
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
