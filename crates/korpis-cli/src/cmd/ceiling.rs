// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis organization ceiling` (§5.1 of docs/08-identity.md): the longest a grant beneath a
//! set subject lasts at an organization, its own beside the one in force.

use super::{Answer, json, mutation, seconds};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use buffa_types::google::protobuf::Duration;
use korpis_proto::korpis::v1 as pb;

fn days(d: Option<&Duration>) -> String {
    match d.map(|d| d.seconds) {
        Some(s) if s % 86_400 == 0 => format!("{}d", s / 86_400),
        Some(s) => format!("{}h", s / 3600),
        None => "none".into(),
    }
}

fn shown(org: &str, c: &pb::DelegationCeiling) -> String {
    format!(
        "{org}: a grant beneath a set subject lasts at most {} from its issue here\n  its own: \
         {}\n",
        days(c.effective.as_option()),
        days(c.own.as_option()),
    )
}

/// Answers `korpis organization ceiling`.
pub async fn ceiling(
    c: &Connection,
    organization: Option<&str>,
    (set, remove): (Option<&str>, bool),
) -> Result<Answer, CliError> {
    let client = pb::OrganizationServiceClient::new(c.http(), c.config());
    let org = resolve::organization(c, organization).await?;
    let ceiling = if let Some(text) = set {
        let request = pb::SetDelegationCeilingRequest {
            mutation: mutation(None).into(),
            organization: org.clone(),
            ceiling: Some(Duration { seconds: seconds(text)?, ..Default::default() }).into(),
            ..Default::default()
        };
        client.set_delegation_ceiling(request).await?.into_owned().ceiling
    } else if remove {
        let request = pb::SetDelegationCeilingRequest {
            mutation: mutation(None).into(),
            organization: org.clone(),
            ..Default::default()
        };
        client.set_delegation_ceiling(request).await?.into_owned().ceiling
    } else {
        let request =
            pb::GetDelegationCeilingRequest { organization: org.clone(), ..Default::default() };
        client.get_delegation_ceiling(request).await?.into_owned().ceiling
    };
    let ceiling = ceiling.into_option().unwrap_or_default();
    Ok(Answer { human: shown(&org, &ceiling), json: json(&ceiling) })
}
