// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis grant share` (§6.1 of docs/08-identity.md, §7 of docs/13-surface-cli.md): a link-form
//! grant, answered as a URL whose fragment is the link. Holding the URL is holding the authority,
//! so the link expires and limits its uses whatever is asked, and the command says what a use is
//! and what it costs before anyone sends it.

use super::grant::object_scope;
use super::{Answer, json, mutation, seconds};
use crate::connect::Connection;
use crate::error::CliError;
use buffa_types::google::protobuf::Timestamp;
use korpis_proto::korpis::v1 as pb;

/// `korpis grant share`.
#[derive(Debug, Clone, clap::Args)]
pub struct Share {
    /// What it permits, comma-separated, e.g. `workload.restart`.
    #[arg(long, value_delimiter = ',', required = true)]
    pub actions: Vec<String>,
    /// The one object it is for, as for `grant issue --scope`, e.g. `workload:mc-survival`.
    #[arg(long)]
    pub scope: String,
    /// How long it lasts, as `30m`, `12h`, or `7d`.
    #[arg(long, default_value = "24h")]
    pub expires: String,
    /// How many times it may be used: each call it authorizes, or each session it opens, is one.
    #[arg(long, default_value_t = 3)]
    pub uses: u64,
    /// What it is for, shown wherever it is listed.
    #[arg(long, default_value = "")]
    pub label: String,
    /// The grant to attenuate; the one you hold that it is valid from when absent.
    #[arg(long)]
    pub parent: Option<String>,
}

/// The URL a link is sent as: the control plane's, with the link in the fragment, which a
/// browser never sends to a server (§6 of docs/11-surface-web.md).
#[must_use]
pub fn url(server: &str, link: &str) -> String {
    format!("{server}/g/#{link}")
}

async fn request(c: &Connection, s: &Share) -> Result<pb::IssueGrantRequest, CliError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| CliError::Local(e.to_string()))?;
    let now = i64::try_from(now.as_secs()).map_err(|e| CliError::Local(e.to_string()))?;
    let expires_at =
        Timestamp { seconds: now.saturating_add(seconds(&s.expires)?), ..Default::default() };
    let conditions = pb::Conditions {
        expires_at: Some(expires_at).into(),
        max_uses: Some(s.uses),
        ..Default::default()
    };
    Ok(pb::IssueGrantRequest {
        mutation: mutation(None).into(),
        parent: s.parent.clone().unwrap_or_default(),
        actions: s.actions.clone(),
        scope: object_scope(c, &s.scope).await?.into(),
        conditions: conditions.into(),
        label: s.label.clone(),
        link_form: true,
        ..Default::default()
    })
}

/// Answers `korpis grant share`: the link, once.
pub async fn share(c: &Connection, s: &Share) -> Result<Answer, CliError> {
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let r = client.issue_grant(request(c, s).await?).await?.into_owned();
    let link = url(c.server(), &r.link_token);
    let grant = r.grant.as_option().map(|g| g.id.clone()).unwrap_or_default();
    let human = format!(
        "{link}\n\nWhoever holds this link holds {actions} on {scope}, {uses} times, for \
         {expires}; each call or session it is used for spends one, and using it needs the \
         control plane. It is shown once. `korpis grant revoke {grant}` ends it.\n",
        actions = s.actions.join(","),
        scope = s.scope,
        uses = s.uses,
        expires = s.expires,
    );
    let mut value = json(&r);
    if let Some(o) = value.as_object_mut() {
        o.insert("url".into(), link.into());
    }
    Ok(Answer { json: value, human })
}

#[cfg(test)]
mod tests {
    use crate::connect::credential;

    #[test]
    fn a_pasted_link_presents_its_fragment_and_anything_else_itself() {
        let u = super::url("https://korpis.example:7443", "korpislk_abc");
        assert_eq!(u, "https://korpis.example:7443/g/#korpislk_abc");
        assert_eq!(credential(&u), "korpislk_abc");
        assert_eq!(credential("korpislk_abc"), "korpislk_abc");
        assert_eq!(credential("korpis_secret"), "korpis_secret");
        assert_eq!(credential("https://x/#other"), "https://x/#other");
    }
}
