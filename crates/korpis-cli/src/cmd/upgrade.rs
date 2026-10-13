// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis extension upgrade` (§7 of docs/13-surface-cli.md, §9 question 6 of
//! docs/16-extensions.md): without `--approve` it shows what the new version needs and what the
//! upgrade would grant and revoke, and the command that approves exactly that digest; with it, it
//! upgrades the installation in place.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::show::short;
use korpis_proto::korpis::v1 as pb;
use std::fmt::Write as _;

/// What a dry run shows the upgrader, and the command that approves exactly it.
fn review(r: &pb::UpgradeExtensionResponse, extension: &str) -> String {
    let e = r.extension.as_option().cloned().unwrap_or_default();
    let mut out = format!("{} {} -> {} ({})\n", e.name, r.from_version, e.version, e.api_version);
    let _ = writeln!(out, "\nneeds:");
    for n in e.manifest.as_option().map(|m| m.needs.clone()).unwrap_or_default() {
        let scope = short(n.scope, "NEED_SCOPE_").to_lowercase();
        let _ = writeln!(out, "  {} over {scope}: {}", n.actions.join(", "), n.reason);
    }
    let _ = writeln!(
        out,
        "\ngrants: {} kept, {} to issue, {} to revoke",
        e.grants.len().saturating_sub(r.issued_grants.len()),
        r.issued_grants.len(),
        r.revoked_grants.len()
    );
    for g in &r.revoked_grants {
        let _ = writeln!(out, "  revoke {g}");
    }
    let digest = e.artifact.as_option().map(|a| a.digest.clone()).unwrap_or_default();
    let again = format!("korpis extension upgrade {extension} --version {digest} --approve");
    let _ = writeln!(out, "\nto grant these and upgrade it:\n  {again}");
    out
}

/// Answers `korpis extension upgrade`.
pub async fn upgrade(
    c: &Connection,
    extension: &str,
    version: &str,
    approve: bool,
) -> Result<Answer, CliError> {
    let client = pb::ExtensionServiceClient::new(c.http(), c.config());
    let base = pb::UpgradeExtensionRequest {
        mutation: mutation(None).into(),
        id: extension.to_owned(),
        version: version.to_owned(),
        ..Default::default()
    };
    let dry = pb::UpgradeExtensionRequest { dry_run: true, ..base.clone() };
    let shown = client.upgrade_extension(dry).await?.into_owned();
    if !approve {
        return Ok(Answer { human: review(&shown, extension), json: json(&shown) });
    }
    let e = shown.extension.as_option().cloned().unwrap_or_default();
    let digest = e.artifact.as_option().map(|a| a.digest.clone()).unwrap_or_default();
    if digest != version {
        let why = format!(
            "--approve grants what one digest declares; run without it, read the needs, and \
             approve with --version {digest}"
        );
        return Err(CliError::Local(why));
    }
    let needs = e.manifest.as_option().map(|m| m.needs.clone()).unwrap_or_default();
    let request = pb::UpgradeExtensionRequest { approved_needs: needs, ..base };
    let done = client.upgrade_extension(request).await?.into_owned();
    let to = done.extension.as_option().map(|e| e.version.clone()).unwrap_or_default();
    let plan =
        done.plan.as_option().map_or_else(String::new, |p| format!("; its plan is {}", p.id));
    let human = format!("upgraded {extension} from {} to {to}{plan}\n", done.from_version);
    Ok(Answer { human, json: json(&done) })
}
