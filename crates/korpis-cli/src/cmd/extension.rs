// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis extension install|list|show|uninstall` (§8 of docs/13-surface-cli.md, §4 of
//! docs/16-extensions.md). Installing is two steps, as planning and applying are: without
//! `--approve` it shows what the extension needs and the command that approves exactly that, the
//! artifact pinned by digest; with it, it installs, granting what the document at that digest
//! declares.

use super::{Answer, json, mutation, pairs};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind as V;
use std::fmt::Write as _;

/// `korpis extension …`.
#[derive(Debug, Clone, Subcommand)]
pub enum ExtensionAction {
    /// Shows what an extension needs, or, with `--approve`, installs it.
    Install(Box<Install>),
    /// The extensions installed in an organization.
    List {
        /// The organization, by name or identifier; the top one you can read by default.
        #[arg(long)]
        organization: Option<String>,
    },
    /// One installation: what it declares, holds, and runs as.
    Show {
        /// The installation, `ext_…`.
        extension: String,
    },
    /// Revokes an installation's grants and proposes its workload's retirement.
    Uninstall {
        /// The installation, `ext_…`.
        extension: String,
    },
}

/// `korpis extension install`.
#[derive(Debug, Clone, clap::Args)]
pub struct Install {
    /// The artifact's repository, e.g. `registry.example/korpis/cloudflare-dns`.
    pub artifact: String,
    /// A tag, or a digest; `--approve` wants the digest the first step printed.
    #[arg(long)]
    pub version: String,
    /// The organization, by name or identifier; the top one you can read by default.
    #[arg(long)]
    pub organization: Option<String>,
    /// Installs it into this project rather than the whole organization.
    #[arg(long)]
    pub project: Option<String>,
    /// The project its workload runs in, when it is installed into the organization.
    #[arg(long)]
    pub workload_project: Option<String>,
    /// Its workload's name; the extension's own by default.
    #[arg(long)]
    pub name: Option<String>,
    /// A configuration value, `key=value`; the value is read as JSON when it parses; repeatable.
    #[arg(long = "config")]
    pub config: Vec<String>,
    /// Grants what it needs and installs it.
    #[arg(long)]
    pub approve: bool,
}

fn value(text: &str) -> pb::Value {
    let kind = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Bool(b)) => V::BoolValue(b),
        Ok(serde_json::Value::Number(n)) if n.is_i64() => V::IntValue(n.as_i64().unwrap_or(0)),
        Ok(serde_json::Value::Number(n)) => V::DoubleValue(n.as_f64().unwrap_or(0.0)),
        _ => V::StringValue(text.to_owned()),
    };
    pb::Value { kind: Some(kind), ..Default::default() }
}

async fn request(c: &Connection, i: &Install) -> Result<pb::InstallExtensionRequest, CliError> {
    let project = match &i.project {
        Some(p) => Some(resolve::project(c, p).await?),
        None => None,
    };
    let workload_project = match &i.workload_project {
        Some(p) => resolve::project(c, p).await?,
        None => String::new(),
    };
    Ok(pb::InstallExtensionRequest {
        mutation: mutation(None).into(),
        artifact: i.artifact.clone(),
        version: i.version.clone(),
        organization: resolve::organization(c, i.organization.as_deref()).await?,
        project,
        workload_project,
        workload_name: i.name.clone().unwrap_or_default(),
        config: pairs("--config", &i.config)?.iter().map(|(k, v)| (k.clone(), value(v))).collect(),
        ..Default::default()
    })
}

/// What a dry run shows the installer, and the command that approves exactly it.
fn review(e: &pb::Extension, i: &Install) -> String {
    let mut out = format!("{} {} ({})\n", e.name, e.version, e.api_version);
    let m = e.manifest.as_option().cloned().unwrap_or_default();
    let _ = writeln!(out, "\nneeds:");
    for n in &m.needs {
        let scope = short(n.scope, "NEED_SCOPE_").to_lowercase();
        let _ = writeln!(out, "  {} over {scope}: {}", n.actions.join(", "), n.reason);
    }
    let lists = [("reaches", m.egress.clone()), ("requires", m.requires.clone())];
    for (what, items) in lists.iter().filter(|(_, l)| !l.is_empty()) {
        let _ = writeln!(out, "{what}: {}", items.join(", "));
    }
    for p in &m.provides {
        let _ = writeln!(
            out,
            "provides: {} {}",
            short(p.r#type, "PROVIDER_TYPE_").to_lowercase(),
            p.name
        );
    }
    for f in &m.subscribes {
        let _ = writeln!(out, "subscribes: {}", f.kinds.join(", "));
    }
    let digest = e.artifact.as_option().map(|a| a.digest.clone()).unwrap_or_default();
    let mut again = format!("korpis extension install {} --version {digest} --approve", i.artifact);
    for (flag, v) in [("--organization", &i.organization), ("--project", &i.project)] {
        if let Some(v) = v {
            let _ = write!(again, " {flag} {v}");
        }
    }
    for (flag, v) in [("--workload-project", &i.workload_project), ("--name", &i.name)] {
        if let Some(v) = v {
            let _ = write!(again, " {flag} {v}");
        }
    }
    for c in &i.config {
        let _ = write!(again, " --config {c}");
    }
    let _ = writeln!(out, "\nto grant these and install it:\n  {again}");
    out
}

fn rows(list: &[pb::Extension]) -> String {
    let mut all = vec![
        ["ID", "NAME", "VERSION", "STATUS", "WORKLOAD", "INSTALLED"].map(str::to_owned).to_vec(),
    ];
    all.extend(list.iter().map(|e| {
        vec![
            e.id.clone(),
            e.name.clone(),
            e.version.clone(),
            short(e.status, "EXTENSION_STATUS_"),
            e.workload.clone(),
            time(e.installed_at.as_option()),
        ]
    }));
    table(&all)
}

/// Answers `korpis extension install`.
async fn install(c: &Connection, i: &Install) -> Result<Answer, CliError> {
    let client = pb::ExtensionServiceClient::new(c.http(), c.config());
    let base = request(c, i).await?;
    let dry = pb::InstallExtensionRequest { dry_run: true, ..base.clone() };
    let shown = client.install_extension(dry).await?.into_owned();
    let extension = shown.extension.into_option().unwrap_or_default();
    if !i.approve {
        return Ok(Answer { human: review(&extension, i), json: json(&extension) });
    }
    let digest = extension.artifact.as_option().map(|a| a.digest.as_str()).unwrap_or_default();
    if digest != i.version {
        let why = format!(
            "--approve grants what one digest declares; run without it, read the needs, and \
             approve with --version {digest}"
        );
        return Err(CliError::Local(why));
    }
    let needs = extension.manifest.as_option().map(|m| m.needs.clone()).unwrap_or_default();
    let installed = client
        .install_extension(pb::InstallExtensionRequest { approved_needs: needs, ..base })
        .await?;
    let installed = installed.into_owned();
    let e = installed.extension.as_option().cloned().unwrap_or_default();
    let human = format!("installed {} as {}\nits workload: {}\n", e.name, e.id, e.workload);
    Ok(Answer { human, json: json(&installed) })
}

/// Answers `korpis extension …`.
pub async fn extension(c: &Connection, action: &ExtensionAction) -> Result<Answer, CliError> {
    let client = pb::ExtensionServiceClient::new(c.http(), c.config());
    match action {
        ExtensionAction::Install(i) => install(c, i).await,
        ExtensionAction::List { organization } => {
            let organization = resolve::organization(c, organization.as_deref()).await?;
            let request = pb::ListExtensionsRequest { organization, ..Default::default() };
            let list = client.list_extensions(request).await?.into_owned().extensions;
            Ok(Answer { human: rows(&list), json: json(&list) })
        }
        ExtensionAction::Show { extension } => {
            let request = pb::GetExtensionRequest { id: extension.clone(), ..Default::default() };
            let e = client.get_extension(request).await?.into_owned().extension;
            let e = e.into_option().unwrap_or_default();
            let mut human = rows(std::slice::from_ref(&e));
            let _ = writeln!(human, "grants: {}", e.grants.join(", "));
            Ok(Answer { human, json: json(&e) })
        }
        ExtensionAction::Uninstall { extension } => {
            let request = pb::UninstallExtensionRequest {
                mutation: mutation(None).into(),
                id: extension.clone(),
                ..Default::default()
            };
            let r = client.uninstall_extension(request).await?.into_owned();
            let plan = r.plan.as_option().map_or_else(String::new, |p| {
                format!("; its workload's retirement waits for approval as plan {}", p.id)
            });
            Ok(Answer { human: format!("uninstalled {extension}{plan}\n"), json: json(&r) })
        }
    }
}
