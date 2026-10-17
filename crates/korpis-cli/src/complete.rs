// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shell completion that resolves against what the caller can read (§7 of
//! docs/13-surface-cli.md): every candidate is a name the API listed for the caller, through the
//! same reads as name resolution, so completion is never a name-disclosure oracle. The shell asks
//! the binary on each completion; nothing is cached, so a grant revoked a moment ago completes
//! nothing.
//!
//! A completer sees the environment, not the command line being typed: it connects with
//! `KORPIS_SERVER`, `KORPIS_CA`, and `KORPIS_TOKEN` or `KORPIS_TOKEN_FILE`, or the session
//! `korpis login` kept for that server. Anything that fails
//! completes nothing, silently; a completion is never an error message in the middle of a line.

use crate::connect::{Connection, token_from};
use crate::error::CliError;
use crate::resolve;
use crate::session;
use clap_complete::engine::CompletionCandidate;
use korpis_proto::korpis::v1 as pb;
use std::ffi::OsStr;
use std::future::Future;
use std::path::PathBuf;

/// The scope kinds `--scope` takes, in the order they are offered.
const SCOPE_KINDS: [&str; 7] =
    ["org", "project", "workload", "node", "volume", "endpoint", "stream"];

/// The connection the environment describes: its token, or the session `korpis login` kept.
async fn connection() -> Option<Connection> {
    let server = std::env::var("KORPIS_SERVER").ok()?;
    let ca = PathBuf::from(std::env::var_os("KORPIS_CA")?);
    let token = match std::env::var_os("KORPIS_TOKEN_FILE") {
        Some(path) => Some(token_from(&PathBuf::from(path)).ok()?),
        None => std::env::var("KORPIS_TOKEN").ok(),
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    session::connection(&server, ca, token, None).await.ok()
}

/// The candidates `list` finds that start with `current`, each a value and what it is.
fn complete<F, Fut>(current: &OsStr, list: F) -> Vec<CompletionCandidate>
where
    F: FnOnce(Connection) -> Fut,
    Fut: Future<Output = Result<Vec<(String, String)>, CliError>>,
{
    let Some(prefix) = current.to_str() else { return Vec::new() };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
    let listed = async {
        let c = connection().await.ok_or(())?;
        list(c).await.map_err(|_| ())
    };
    let Ok(found) = runtime.map_err(|_| ()).and_then(|r| r.block_on(listed)) else {
        return Vec::new();
    };
    found
        .into_iter()
        .filter(|(value, _)| value.starts_with(prefix))
        .map(|(value, help)| CompletionCandidate::new(value).help(Some(help.into())))
        .collect()
}

async fn organization_names(c: Connection) -> Result<Vec<(String, String)>, CliError> {
    Ok(resolve::organizations(&c).await?.into_iter().map(|o| (o.name, o.id)).collect())
}

async fn project_names(c: Connection) -> Result<Vec<(String, String)>, CliError> {
    Ok(resolve::readable_projects(&c).await?.into_iter().map(|p| (p.name, p.id)).collect())
}

/// Workload names, written `project/name` where the bare name is not unique.
async fn workload_names(c: Connection) -> Result<Vec<(String, String)>, CliError> {
    let found = resolve::readable_workloads(&c, None).await?;
    let unique = |name: &str| found.iter().filter(|(_, w)| w.name == name).count() == 1;
    Ok(found
        .iter()
        .map(|(project, w)| {
            let value =
                if unique(&w.name) { w.name.clone() } else { format!("{project}/{}", w.name) };
            (value, format!("{} in {project}", w.id))
        })
        .collect())
}

async fn node_names(c: Connection) -> Result<Vec<(String, String)>, CliError> {
    Ok(resolve::readable_nodes(&c).await?.into_iter().map(|n| (n.name, n.id)).collect())
}

/// Plans that wait for a decision or for their turn, on every workload the caller may read.
async fn plan_ids(c: Connection) -> Result<Vec<(String, String)>, CliError> {
    let client = pb::PlanServiceClient::new(c.http(), c.config());
    let waiting = [pb::PlanStatus::PLAN_STATUS_PROPOSED, pb::PlanStatus::PLAN_STATUS_APPROVED];
    let mut out = Vec::new();
    for (project, w) in resolve::readable_workloads(&c, None).await? {
        let request = pb::ListPlansRequest { workload: w.id.clone(), ..Default::default() };
        let listed = resolve::readable(client.list_plans(request).await.map_err(CliError::from))?;
        let plans = listed.map(|r| r.into_owned().plans).unwrap_or_default();
        out.extend(
            plans
                .into_iter()
                .filter(|p| p.status.as_known().is_some_and(|s| waiting.contains(&s)))
                .map(|p| (p.id, format!("{project}/{}", w.name))),
        );
    }
    Ok(out)
}

/// The grants the caller holds, and those anchored at the organizations and projects it may
/// read where it may read grants.
async fn grant_ids(c: Connection) -> Result<Vec<(String, String)>, CliError> {
    use korpis_proto::korpis::v1::__buffa::oneof::{list_grants_request::Filter, scope::Kind};
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let mut filters = vec![None];
    for o in resolve::organization_ids(&c).await? {
        filters.push(Some(Kind::Organization(o)));
    }
    for p in resolve::readable_projects(&c).await? {
        filters.push(Some(Kind::Project(p.id)));
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for kind in filters {
        let filter = kind
            .map(|k| Filter::Scope(Box::new(pb::Scope { kind: Some(k), ..Default::default() })));
        let request = pb::ListGrantsRequest { filter, ..Default::default() };
        let listed = resolve::readable(client.list_grants(request).await.map_err(CliError::from))?;
        for g in listed.map(|r| r.into_owned().grants).unwrap_or_default() {
            if !out.iter().any(|(id, _)| *id == g.id) {
                out.push((g.id, g.label));
            }
        }
    }
    Ok(out)
}

/// Completes an organization name.
#[must_use]
pub fn organizations(current: &OsStr) -> Vec<CompletionCandidate> {
    complete(current, organization_names)
}

/// Completes a project name.
#[must_use]
pub fn projects(current: &OsStr) -> Vec<CompletionCandidate> {
    complete(current, project_names)
}

/// Completes a workload name.
#[must_use]
pub fn workloads(current: &OsStr) -> Vec<CompletionCandidate> {
    complete(current, workload_names)
}

/// Completes a node name.
#[must_use]
pub fn nodes(current: &OsStr) -> Vec<CompletionCandidate> {
    complete(current, node_names)
}

/// Completes a plan identifier.
#[must_use]
pub fn plans(current: &OsStr) -> Vec<CompletionCandidate> {
    complete(current, plan_ids)
}

/// Completes a grant identifier.
#[must_use]
pub fn grants(current: &OsStr) -> Vec<CompletionCandidate> {
    complete(current, grant_ids)
}

/// Completes a scope: its kind, then a name of that kind.
#[must_use]
pub fn scopes(current: &OsStr) -> Vec<CompletionCandidate> {
    let text = current.to_string_lossy();
    let Some((kind, name)) = text.split_once(':') else {
        return SCOPE_KINDS
            .iter()
            .filter(|k| k.starts_with(text.as_ref()))
            .map(|k| CompletionCandidate::new(format!("{k}:")))
            .collect();
    };
    let names = match kind {
        "org" | "organization" => organizations(OsStr::new(name)),
        "project" => projects(OsStr::new(name)),
        "workload" => workloads(OsStr::new(name)),
        "node" => nodes(OsStr::new(name)),
        _ => Vec::new(),
    };
    names
        .into_iter()
        .map(|c| {
            let value = format!("{kind}:{}", c.get_value().to_string_lossy());
            CompletionCandidate::new(value).help(c.get_help().cloned())
        })
        .collect()
}

/// The shells completion can be registered with.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Shell {
    /// Bash.
    Bash,
    /// Zsh.
    Zsh,
    /// Fish.
    Fish,
    /// Elvish.
    Elvish,
    /// PowerShell.
    Powershell,
}

/// The environment variable that asks the binary for completions.
pub const COMPLETE_VAR: &str = "KORPIS_COMPLETE";

/// Writes the registration script for `shell`, pointing at this binary.
pub fn registration(shell: Shell) -> Result<(), CliError> {
    use clap_complete::env::{Bash, Elvish, EnvCompleter, Fish, Powershell, Zsh};
    let completer: &dyn EnvCompleter = match shell {
        Shell::Bash => &Bash,
        Shell::Zsh => &Zsh,
        Shell::Fish => &Fish,
        Shell::Elvish => &Elvish,
        Shell::Powershell => &Powershell,
    };
    let bin = std::env::current_exe().map_err(|e| CliError::Local(e.to_string()))?;
    let bin = bin.to_string_lossy();
    let mut out = std::io::stdout().lock();
    completer
        .write_registration(COMPLETE_VAR, "korpis", &bin, &bin, &mut out)
        .map_err(|e| CliError::Local(e.to_string()))
}
