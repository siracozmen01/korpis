// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis stream share` (§8 of docs/14-streams.md): a share link to read one range of a
//! workload's console, by the node's clock, and nothing else. It is a link-form grant of
//! `workload.console.read` carrying the range (§6.1 of docs/08-identity.md), so the node reading
//! the stream, and the control plane reading its offloaded segments, send only what was written
//! within it.

use super::share::url;
use super::{Answer, json, mutation, seconds, stream};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::scope;
use std::time::SystemTime;

/// `korpis stream`: the stream, or a share of a range of it.
#[derive(Debug, Clone, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct StreamArgs {
    /// The workload: an identifier, `project/name`, or a name.
    pub workload: Option<String>,
    /// What to do instead of showing it.
    #[command(subcommand)]
    pub action: Option<StreamAction>,
}

/// What `korpis stream` does besides showing the stream.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum StreamAction {
    /// Makes a link to read what the workload's console wrote between two times, with no account.
    Share(Share),
}

/// `korpis stream share`.
#[derive(Debug, Clone, clap::Args)]
pub struct Share {
    /// The workload: an identifier, `project/name`, or a name.
    pub workload: String,
    /// The first instant shown: RFC 3339, or a time of day today in the local zone, `14:30`.
    #[arg(long)]
    pub from: String,
    /// The first instant not shown, as for `--from`; now when absent.
    #[arg(long)]
    pub to: Option<String>,
    /// How long the link lasts, as `30m`, `12h`, or `7d`.
    #[arg(long, default_value = "24h")]
    pub expires: String,
    /// How many times it may be opened.
    #[arg(long, default_value_t = 3)]
    pub uses: u64,
}

/// The instant `text` names, against `now`: an RFC 3339 timestamp, or a time of day today in the
/// local zone.
pub fn instant(text: &str, now: &jiff::Zoned) -> Result<jiff::Timestamp, CliError> {
    if let Ok(t) = text.parse::<jiff::Timestamp>() {
        return Ok(t);
    }
    let bad = |e: &dyn std::fmt::Display| {
        CliError::Local(format!("{text:?}: RFC 3339, or a time of day such as 14:30 ({e})"))
    };
    let time: jiff::civil::Time = text.parse().map_err(|e| bad(&e))?;
    let at = now.date().to_datetime(time).to_zoned(now.time_zone().clone());
    Ok(at.map_err(|e| bad(&e))?.timestamp())
}

fn timestamp(t: jiff::Timestamp) -> buffa_types::google::protobuf::Timestamp {
    buffa_types::google::protobuf::Timestamp {
        seconds: t.as_second(),
        nanos: t.subsec_nanosecond(),
        ..Default::default()
    }
}

async fn request(c: &Connection, s: &Share) -> Result<pb::IssueGrantRequest, CliError> {
    let now = jiff::Zoned::now();
    let from = instant(&s.from, &now)?;
    let to = s.to.as_deref().map_or(Ok(now.timestamp()), |t| instant(t, &now))?;
    if from >= to {
        return Err(CliError::Local("--from is before --to".into()));
    }
    let expires = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| CliError::Local(e.to_string()))?;
    let expires = i64::try_from(expires.as_secs()).map_err(|e| CliError::Local(e.to_string()))?;
    let conditions = pb::Conditions {
        expires_at: Some(buffa_types::google::protobuf::Timestamp {
            seconds: expires.saturating_add(seconds(&s.expires)?),
            ..Default::default()
        })
        .into(),
        max_uses: Some(s.uses),
        stream_range: Some(pb::TimeRange {
            from: Some(timestamp(from)).into(),
            to: Some(timestamp(to)).into(),
            ..Default::default()
        })
        .into(),
        ..Default::default()
    };
    let workload = resolve::workload(c, &s.workload).await?;
    let kind = scope::Kind::Workload(workload);
    Ok(pb::IssueGrantRequest {
        mutation: mutation(None).into(),
        actions: vec!["workload.console.read".into()],
        scope: pb::Scope { kind: Some(kind), ..Default::default() }.into(),
        conditions: conditions.into(),
        label: format!("{} console, {from} to {to}", s.workload),
        link_form: true,
        ..Default::default()
    })
}

/// Answers `korpis stream share`: the link, once, and how its recipient reads it.
async fn share(c: &Connection, s: &Share) -> Result<Answer, CliError> {
    let request = request(c, s).await?;
    let workload = match request.scope.as_option().and_then(|s| s.kind.as_ref()) {
        Some(scope::Kind::Workload(w)) => w.clone(),
        _ => String::new(),
    };
    let client = pb::GrantServiceClient::new(c.http(), c.config());
    let r = client.issue_grant(request).await?.into_owned();
    let link = url(c.server(), &r.link_token);
    let human = format!(
        "{link}\n\nWhoever holds this link reads {label}, {uses} times, for {expires}; each \
         reading spends one, and needs the control plane. It is shown once. With the CLI: \
         KORPIS_TOKEN='<the link>' korpis logs {workload}\n",
        label = r.grant.as_option().map(|g| g.label.as_str()).unwrap_or_default(),
        uses = s.uses,
        expires = s.expires,
    );
    let mut value = json(&r);
    if let Some(o) = value.as_object_mut() {
        o.insert("url".into(), link.into());
    }
    Ok(Answer { json: value, human })
}

/// Answers `korpis stream …`.
pub async fn run(c: &Connection, a: &StreamArgs) -> Result<Answer, CliError> {
    match (&a.action, &a.workload) {
        (Some(StreamAction::Share(s)), _) => share(c, s).await,
        (None, Some(w)) => stream::stream(c, w).await,
        (None, None) => Err(CliError::Local("name a workload, or `share`".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_is_rfc_3339_or_a_time_of_day_in_the_local_zone() {
        let tz = jiff::tz::TimeZone::get("Europe/Istanbul").unwrap();
        let now = jiff::civil::date(2026, 10, 14).at(18, 0, 0, 0).to_zoned(tz).unwrap();
        let at = instant("14:30", &now).unwrap();
        assert_eq!(at.to_string(), "2026-10-14T11:30:00Z", "Istanbul is three hours ahead");
        let exact = instant("2026-10-14T14:30:00Z", &now).unwrap();
        assert_eq!(exact.to_string(), "2026-10-14T14:30:00Z");
        assert!(instant("half past two", &now).is_err());
    }
}
