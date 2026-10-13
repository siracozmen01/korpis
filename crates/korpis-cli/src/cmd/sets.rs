// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis set list|mode` (§6 of docs/13-surface-cli.md): a project's declared sets, and what
//! each does with drift. A set is created by the first `apply --set` through it.

use super::{Answer, Mode, SetAction, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use korpis_proto::korpis::v1 as pb;

async fn sets(c: &Connection, project: &str) -> Result<Vec<pb::DeclaredSet>, CliError> {
    let client = pb::DeclaredSetServiceClient::new(c.http(), c.config());
    let request = pb::ListDeclaredSetsRequest { project: project.to_owned(), ..Default::default() };
    Ok(client.list_declared_sets(request).await?.into_owned().sets)
}

fn rows(list: &[pb::DeclaredSet]) -> String {
    let mut all = vec![["ID", "NAME", "MODE", "VERSION", "CREATED"].map(str::to_owned).to_vec()];
    all.extend(list.iter().map(|s| {
        vec![
            s.id.clone(),
            s.name.clone(),
            short(s.mode, "DRIFT_MODE_"),
            s.version.to_string(),
            time(s.created_at.as_option()),
        ]
    }));
    table(&all)
}

/// Answers `korpis set …`.
pub async fn set(c: &Connection, action: &SetAction) -> Result<Answer, CliError> {
    match action {
        SetAction::List { project } => {
            let list = sets(c, &resolve::project(c, project).await?).await?;
            Ok(Answer { human: rows(&list), json: json(&list) })
        }
        SetAction::Mode { set, mode } => {
            let (project, name) = set
                .split_once('/')
                .ok_or_else(|| CliError::Local(format!("{set:?}: a set is project/name")))?;
            let project = resolve::project(c, project).await?;
            let current = sets(c, &project).await?.into_iter().find(|s| s.name == name);
            let current = current
                .ok_or_else(|| CliError::Local(format!("no set {name:?} in that project")))?;
            let request = pb::SetDeclaredSetModeRequest {
                mutation: mutation(Some(current.version)).into(),
                project,
                name: name.to_owned(),
                mode: match mode {
                    Mode::Advisory => pb::DriftMode::DRIFT_MODE_ADVISORY,
                    Mode::Strict => pb::DriftMode::DRIFT_MODE_STRICT,
                }
                .into(),
                ..Default::default()
            };
            let client = pb::DeclaredSetServiceClient::new(c.http(), c.config());
            let r = client.set_declared_set_mode(request).await?.into_owned();
            let changed: Vec<pb::DeclaredSet> = r.set.as_option().cloned().into_iter().collect();
            Ok(Answer { human: rows(&changed), json: json(&r) })
        }
    }
}
