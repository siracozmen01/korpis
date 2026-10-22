// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis organization placement` (§2.2 of docs/05-scheduling.md): the placement policy at an
//! organization, its own beside the one in force and where that one was set; with `-f` it sets
//! its own from a file, JSON or YAML, as `--json` prints a policy, and `--remove` removes it.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use korpis_proto::korpis::v1 as pb;
use std::path::Path;

/// A policy as a person reads it: its strategy, and how many spread constraints, affinity rules,
/// and weights it states.
fn summary(p: Option<&pb::PlacementPolicy>) -> String {
    let Some(p) = p else { return "none".into() };
    let strategy = match p.strategy.as_known() {
        Some(pb::PlacementStrategy::PLACEMENT_STRATEGY_PACKED) => "packed",
        Some(pb::PlacementStrategy::PLACEMENT_STRATEGY_DISTRIBUTED) => "distributed",
        _ => "balanced",
    };
    format!(
        "{strategy}, {} spread constraints, {} affinity rules, {} weights",
        p.spread.len(),
        p.affinity.len(),
        p.weights.len()
    )
}

fn shown(org: &str, p: &pb::OrganizationPlacementPolicy) -> String {
    let at = if p.set_at.is_empty() { "the installation" } else { p.set_at.as_str() };
    format!(
        "{org}: placed by the policy set at {at}: {}\n  its own: {}\n",
        summary(p.effective.as_option()),
        summary(p.own.as_option()),
    )
}

fn read(file: &Path) -> Result<pb::PlacementPolicy, CliError> {
    let at = file.display();
    let text = std::fs::read_to_string(file).map_err(|e| CliError::Local(format!("{at}: {e}")))?;
    serde_saphyr::from_str(&text).map_err(|e| CliError::Local(format!("{at}: {e}")))
}

/// Answers `korpis organization placement`.
pub async fn placement(
    c: &Connection,
    organization: Option<&str>,
    (file, remove): (Option<&Path>, bool),
) -> Result<Answer, CliError> {
    let client = pb::OrganizationServiceClient::new(c.http(), c.config());
    let org = resolve::organization(c, organization).await?;
    let policy = if file.is_some() || remove {
        let request = pb::SetPlacementPolicyRequest {
            mutation: mutation(None).into(),
            organization: org.clone(),
            policy: file.map(read).transpose()?.into(),
            ..Default::default()
        };
        client.set_placement_policy(request).await?.into_owned().policy
    } else {
        let request =
            pb::GetPlacementPolicyRequest { organization: org.clone(), ..Default::default() };
        client.get_placement_policy(request).await?.into_owned().policy
    };
    let policy = policy.into_option().unwrap_or_default();
    Ok(Answer { human: shown(&org, &policy), json: json(&policy) })
}
