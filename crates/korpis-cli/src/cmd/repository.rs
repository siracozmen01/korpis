// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis repository …` (§5.3 of docs/06-storage.md): where an organization's backups are kept,
//! at what place in which backup target, and how long.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;

/// `korpis repository …`.
#[derive(Debug, Clone, Subcommand)]
pub enum RepositoryAction {
    /// Makes a repository; its key is held by the control plane.
    Create {
        /// Its name, unique in the organization.
        name: String,
        /// The organization's backup target provider, by name.
        #[arg(long)]
        provider: String,
        /// Its place in the target, a relative prefix ending in '/'.
        #[arg(long)]
        prefix: String,
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
        /// The project it serves, by name or identifier; the whole organization by default.
        #[arg(long)]
        project: Option<String>,
        /// The most recent backups kept whatever their age.
        #[arg(long, default_value_t = 0)]
        keep_last: u32,
        /// The last backup of each of this many days.
        #[arg(long, default_value_t = 7)]
        keep_daily: u32,
        /// The last backup of each of this many weeks.
        #[arg(long, default_value_t = 4)]
        keep_weekly: u32,
        /// The last backup of each of this many months.
        #[arg(long, default_value_t = 6)]
        keep_monthly: u32,
        /// A floor beneath which nothing prunes, in days.
        #[arg(long, default_value_t = 1)]
        min_retention_days: u32,
    },
    /// The repositories of an organization, or those serving a project.
    List {
        /// The organization, by name or identifier.
        #[arg(long)]
        organization: Option<String>,
        /// Only those serving this project.
        #[arg(long)]
        project: Option<String>,
    },
    /// One repository.
    Get {
        /// The repository, `rep_…`.
        repository: String,
    },
}

fn rows(list: &[pb::Repository]) -> String {
    let mut all = vec![
        ["ID", "NAME", "PROVIDER", "PREFIX", "PROJECT", "CUSTODY", "MADE"]
            .map(str::to_owned)
            .to_vec(),
    ];
    all.extend(list.iter().map(|r| {
        let custody = crate::show::short(r.key_custody, "KEY_CUSTODY_").to_lowercase();
        let project = r.project.clone().unwrap_or_else(|| "-".into());
        vec![
            r.id.clone(),
            r.name.clone(),
            r.provider.clone(),
            r.prefix.clone(),
            project,
            custody,
            time(r.created.as_option()),
        ]
    }));
    table(&all)
}

async fn project(c: &Connection, named: Option<&str>) -> Result<Option<String>, CliError> {
    match named {
        Some(p) => Ok(Some(resolve::project(c, p).await?)),
        None => Ok(None),
    }
}

/// The retention a `create` asks for.
fn retention(action: &RepositoryAction) -> pb::RetentionPolicy {
    let RepositoryAction::Create {
        keep_last,
        keep_daily,
        keep_weekly,
        keep_monthly,
        min_retention_days,
        ..
    } = action
    else {
        return pb::RetentionPolicy::default();
    };
    let days = i64::from(*min_retention_days) * 86_400;
    pb::RetentionPolicy {
        keep_last: *keep_last,
        keep_daily: *keep_daily,
        keep_weekly: *keep_weekly,
        keep_monthly: *keep_monthly,
        min_retention: buffa_types::google::protobuf::Duration {
            seconds: days,
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

/// Answers `korpis repository …`.
pub async fn repository(c: &Connection, action: &RepositoryAction) -> Result<Answer, CliError> {
    let client = pb::RepositoryServiceClient::new(c.http(), c.config());
    Ok(match action {
        RepositoryAction::Create { name, provider, prefix, organization, project: p, .. } => {
            let r = pb::CreateRepositoryRequest {
                mutation: mutation(None).into(),
                organization: resolve::organization(c, organization.as_deref()).await?,
                project: project(c, p.as_deref()).await?,
                name: name.clone(),
                provider: provider.clone(),
                prefix: prefix.clone(),
                retention: retention(action).into(),
                ..Default::default()
            };
            let made = client
                .create_repository(r)
                .await?
                .into_owned()
                .repository
                .into_option()
                .unwrap_or_default();
            Answer { human: rows(std::slice::from_ref(&made)), json: json(&made) }
        }
        RepositoryAction::List { organization, project: p } => {
            let r = pb::ListRepositoriesRequest {
                organization: resolve::organization(c, organization.as_deref()).await?,
                project: project(c, p.as_deref()).await?,
                ..Default::default()
            };
            let list = client.list_repositories(r).await?.into_owned().repositories;
            Answer { human: rows(&list), json: json(&list) }
        }
        RepositoryAction::Get { repository } => {
            let r = pb::GetRepositoryRequest { id: repository.clone(), ..Default::default() };
            let got = client
                .get_repository(r)
                .await?
                .into_owned()
                .repository
                .into_option()
                .unwrap_or_default();
            Answer { human: rows(std::slice::from_ref(&got)), json: json(&got) }
        }
    })
}
