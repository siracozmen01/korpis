// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis extension policy [--organization <root>]` shows the rule an operator keeps over the
//! extensions installed beneath a root organization; with `--set` it replaces it with what the
//! flags give, every flag left out going back to its default, open.

use super::{Answer, json, mutation, pairs};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use korpis_proto::korpis::v1 as pb;
use std::fmt::Write as _;

/// `korpis extension policy`.
#[derive(Debug, Clone, clap::Args)]
pub struct Policy {
    /// The root organization, by name or identifier; the top one you can read by default.
    #[arg(long)]
    pub organization: Option<String>,
    /// Replaces the policy with what the flags below give.
    #[arg(long)]
    pub set: bool,
    /// A registry host, or a repository prefix beneath one, extensions may come from; repeat.
    #[arg(long = "registry")]
    pub registries: Vec<String>,
    /// A publisher one of whom must have signed an extension, `name=path/to/cosign.pub`; repeat.
    #[arg(long = "publisher")]
    pub publishers: Vec<String>,
    /// Only those with extension.install over the root organization may install extensions.
    #[arg(long)]
    pub operator_only: bool,
}

fn shown(p: &pb::ExtensionPolicy, version: u64) -> String {
    let mut out = String::new();
    let any = |v: &[String]| if v.is_empty() { "any".to_owned() } else { v.join(", ") };
    let _ = writeln!(out, "registries: {}", any(&p.registries));
    let names: Vec<String> =
        p.publishers.iter().map(|s| format!("{} ({})", s.name, s.fingerprint)).collect();
    let _ = writeln!(out, "publishers: {}", any(&names));
    let who = if p.tenants_install { "anyone holding extension.install" } else { "the operator" };
    let _ = writeln!(out, "who installs: {who}");
    let _ = writeln!(out, "version: {version}");
    out
}

fn publishers(given: &[String]) -> Result<Vec<pb::Signer>, CliError> {
    pairs("--publisher", given)?
        .into_iter()
        .map(|(name, path)| {
            let pem = std::fs::read_to_string(&path)
                .map_err(|e| CliError::Local(format!("--publisher {name}: {path}: {e}")))?;
            Ok(pb::Signer { name, public_key_pem: pem, ..Default::default() })
        })
        .collect()
}

/// `korpis extension policy`.
pub async fn policy(c: &Connection, p: &Policy) -> Result<Answer, CliError> {
    let client = pb::ExtensionServiceClient::new(c.http(), c.config());
    let organization = resolve::organization(c, p.organization.as_deref()).await?;
    let get = pb::GetExtensionPolicyRequest { organization: organization.clone(), ..Default::default() };
    let current = client.get_extension_policy(get).await?.into_owned();
    if !p.set {
        let policy = current.policy.as_option().cloned().unwrap_or_default();
        return Ok(Answer { human: shown(&policy, current.version), json: json(&current) });
    }
    let policy = pb::ExtensionPolicy {
        registries: p.registries.clone(),
        publishers: publishers(&p.publishers)?,
        tenants_install: !p.operator_only,
        ..Default::default()
    };
    let request = pb::SetExtensionPolicyRequest {
        mutation: mutation(Some(current.version)).into(),
        organization,
        policy: policy.into(),
        ..Default::default()
    };
    let r = client.set_extension_policy(request).await?.into_owned();
    let set = r.policy.as_option().cloned().unwrap_or_default();
    Ok(Answer { human: shown(&set, r.version), json: json(&r) })
}
