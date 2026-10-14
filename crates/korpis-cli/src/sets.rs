// SPDX-License-Identifier: AGPL-3.0-or-later

//! `--set` (§5 and §6 of docs/13-surface-cli.md): the files are declared through a declared set,
//! members of the set that no file declares any more are proposed for deletion, and drift is
//! found before anything is declared, so an advisory set's apply fails whole rather than half.

use crate::apply::{Mode, Through, declare, existing};
use crate::connect::Connection;
use crate::declared::File;
use crate::error::CliError;
use crate::resolve;
use crate::show::time;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::registry::Registry;
use std::fmt::Write as _;

/// The set flags of `plan` and `apply`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SetArgs {
    /// Declare through this set; members it no longer declares are proposed for deletion,
    /// and their volumes are kept.
    #[arg(long)]
    pub set: Option<String>,
    /// With drift in an advisory set, apply the rest and leave the drifted members as they are.
    #[arg(long, requires = "set", conflicts_with = "reconcile")]
    pub accept_drift: bool,
    /// With drift in an advisory set, declare over it; each discarded change is recorded with
    /// its author.
    #[arg(long, requires = "set")]
    pub reconcile: bool,
    /// Take workloads the files declare that belong to no set into this one.
    #[arg(long, requires = "set")]
    pub adopt: bool,
    /// Delete these volumes of the members deleted, each as `member/volume`; comma-separated.
    #[arg(long = "delete-volumes", requires = "set", value_delimiter = ',')]
    pub delete_volumes: Vec<String>,
}

impl SetArgs {
    /// The declaration's passage through the set, when one is named.
    #[must_use]
    pub fn through(&self) -> Option<Through> {
        self.set.as_ref().map(|name| Through {
            name: name.clone(),
            adopt: self.adopt,
            reconcile: self.reconcile,
        })
    }
}

/// A member whose head was declared outside the set, and the file that declares it.
#[derive(Debug, Clone)]
pub struct Drifted {
    /// The file.
    pub file: String,
    /// The workload's name.
    pub workload: String,
    /// How it drifted, as the API reports it.
    pub drift: Option<pb::Drift>,
}

/// The members of `files` that drifted, found by declaring each as a dry run through the set.
pub async fn preflight(
    c: &Connection,
    registry: &Registry,
    files: &[File],
    through: &Through,
) -> Result<Vec<Drifted>, CliError> {
    let mut found = Vec::new();
    for f in files {
        match declare(c, registry, f, Mode::DryRun, Some(through)).await {
            Ok(_) => {}
            Err(CliError::Api { reason, .. }) if reason == "set.drifted" => {
                let project = resolve::project(c, &f.declared.project).await?;
                let status = existing(c, &project, &f.declared.workload).await?;
                found.push(Drifted {
                    file: f.path.display().to_string(),
                    workload: f.declared.workload.clone(),
                    drift: status.and_then(|s| s.drift.into_option()),
                });
            }
            Err(e) => return Err(e),
        }
    }
    Ok(found)
}

fn who(s: Option<&pb::Subject>) -> String {
    use korpis_proto::korpis::v1::__buffa::oneof::subject::Kind as S;
    match s.and_then(|s| s.kind.as_ref()) {
        Some(S::User(id) | S::Token(id) | S::Workload(id) | S::Extension(id)) => id.clone(),
        Some(S::Link(grant)) => format!("link {grant}"),
        Some(S::External(e)) => format!("{} identity", e.provider),
        Some(S::System(_)) => "the control plane".into(),
        None => "someone".into(),
    }
}

/// The refusal of an advisory set's apply that met drift: every drifted member, who changed it
/// and when, and the effect that recorded it.
#[must_use]
pub fn refusal(drifted: &[Drifted]) -> CliError {
    let mut message = String::from("the set drifted; nothing was applied:\n");
    for d in drifted {
        let _ = match &d.drift {
            Some(x) => writeln!(
                message,
                "  {} ({}): changed outside the set by {} at {} (effect {})",
                d.workload,
                d.file,
                who(x.declared_by.as_option()),
                time(x.declared_at.as_option()),
                x.effect.as_deref().unwrap_or("unknown"),
            ),
            None => writeln!(message, "  {} ({})", d.workload, d.file),
        };
    }
    message.push_str("--accept-drift leaves them as they are; --reconcile declares over them");
    CliError::Api {
        code: pb::ErrorCode::ERROR_CODE_CONFLICT,
        reason: "set.drifted".into(),
        field: String::new(),
        message,
    }
}

/// Members of the set, in the projects the files declare into, that no file declares.
pub async fn orphans(
    c: &Connection,
    files: &[File],
    set: &str,
) -> Result<Vec<pb::Workload>, CliError> {
    let mut by_project: Vec<(String, Vec<&str>)> = Vec::new();
    for f in files {
        let project = resolve::project(c, &f.declared.project).await?;
        match by_project.iter_mut().find(|(p, _)| *p == project) {
            Some((_, names)) => names.push(&f.declared.workload),
            None => by_project.push((project, vec![&f.declared.workload])),
        }
    }
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let mut out = Vec::new();
    for (project, declared) in by_project {
        let request = pb::ListWorkloadsRequest {
            project: Some(project),
            set: Some(set.to_owned()),
            ..Default::default()
        };
        let members = client.list_workloads(request).await?.into_owned().workloads;
        out.extend(
            members
                .into_iter()
                .filter_map(|s| s.workload.into_option())
                .filter(|w| !declared.contains(&w.name.as_str())),
        );
    }
    Ok(out)
}

/// Proposes deleting `w` and the volumes of it named in `volumes`: a retire plan, which always
/// waits for a person (§3.3 of docs/01-model.md).
pub async fn propose_deletion(
    c: &Connection,
    w: &pb::Workload,
    volumes: Vec<String>,
) -> Result<pb::Plan, CliError> {
    let request = pb::DeleteWorkloadRequest {
        mutation: crate::cmd::mutation(None).into(),
        workload: w.id.clone(),
        delete_volumes: volumes,
        ..Default::default()
    };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    Ok(client.delete_workload(request).await?.into_owned().plan.into_option().unwrap_or_default())
}

/// Confirms plan `id`, which its proposer may (§3.6 of docs/08-identity.md).
pub async fn confirm(c: &Connection, id: &str) -> Result<pb::Plan, CliError> {
    let request = pb::ApprovePlanRequest {
        mutation: crate::cmd::mutation(None).into(),
        plan: id.to_owned(),
        ..Default::default()
    };
    let client = pb::PlanServiceClient::new(c.http(), c.config());
    Ok(client.approve_plan(request).await?.into_owned().plan.into_option().unwrap_or_default())
}
