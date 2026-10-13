// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis event subscribe|list|delete|follow` (§5 of docs/15-observability.md): a subscription
//! is a position in the event log kept for you; `follow` prints what it is delivered as it
//! comes and, with `--ack`, records each as processed, so the next `follow` resumes after it.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::show::{short, table, time};
use clap::Subcommand;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::subscribe_response::Item;
use serde_json::Value;

/// `korpis event …`.
#[derive(Debug, Clone, Subcommand)]
pub enum EventAction {
    /// Creates a subscription, starting at the end of the log.
    Subscribe {
        /// A kind, or a prefix ending in `.` such as `workload.`; repeatable; every kind when
        /// none is given.
        #[arg(long = "kind")]
        kinds: Vec<String>,
        /// The least severity delivered: `info`, `warning`, or `critical`.
        #[arg(long, default_value = "info")]
        severity: String,
    },
    /// Your subscriptions.
    List,
    /// Deletes one of your subscriptions.
    Delete {
        /// The subscription, `sub_…`.
        subscription: String,
    },
    /// Prints what a subscription is delivered, as it comes, until interrupted.
    Follow {
        /// The subscription, `sub_…`.
        subscription: String,
        /// Records each event printed as processed.
        #[arg(long)]
        ack: bool,
    },
}

fn severity(text: &str) -> Result<pb::Severity, CliError> {
    Ok(match text {
        "info" => pb::Severity::SEVERITY_INFO,
        "warning" => pb::Severity::SEVERITY_WARNING,
        "critical" => pb::Severity::SEVERITY_CRITICAL,
        other => return Err(CliError::Local(format!("{other:?}: info, warning, or critical"))),
    })
}

fn rows(list: &[pb::Subscription]) -> String {
    let mut all = vec![["ID", "KINDS", "ACKED", "CREATED"].map(str::to_owned).to_vec()];
    all.extend(list.iter().map(|s| {
        let kinds = s.filter.as_option().map(|f| f.kinds.join(",")).unwrap_or_default();
        vec![
            s.id.clone(),
            if kinds.is_empty() { "*".into() } else { kinds },
            if s.acked.is_empty() { "-".into() } else { s.acked.clone() },
            time(s.created.as_option()),
        ]
    }));
    table(&all)
}

/// One delivered item as a line.
fn line(item: &Item) -> String {
    match item {
        Item::Event(e) => format!(
            "{} {} {:<8} {} {}#{}",
            e.id,
            time(e.at.as_option()),
            short(e.severity, "SEVERITY_").to_lowercase(),
            e.kind,
            e.key,
            e.seq
        ),
        Item::Gap(g) => format!("-- events lost to {}; resuming at {}", g.reason, g.resumes_at),
    }
}

/// Answers `korpis event subscribe|list|delete`.
pub async fn event(c: &Connection, action: &EventAction) -> Result<Answer, CliError> {
    let client = pb::EventServiceClient::new(c.http(), c.config());
    match action {
        EventAction::Subscribe { kinds, severity: s } => {
            let filter = pb::EventFilter {
                kinds: kinds.clone(),
                min_severity: severity(s)?.into(),
                ..Default::default()
            };
            let request = pb::CreateSubscriptionRequest {
                mutation: mutation(None).into(),
                filter: filter.into(),
                ..Default::default()
            };
            let made = client.create_subscription(request).await?.into_owned().subscription;
            let s = made.into_option().unwrap_or_default();
            Ok(Answer { human: format!("subscribed as {}\n", s.id), json: json(&s) })
        }
        EventAction::List => {
            let list = client.list_subscriptions(pb::ListSubscriptionsRequest::default()).await?;
            let list = list.into_owned().subscriptions;
            Ok(Answer { human: rows(&list), json: json(&list) })
        }
        EventAction::Delete { subscription } => {
            let request = pb::DeleteSubscriptionRequest {
                mutation: mutation(None).into(),
                id: subscription.clone(),
                ..Default::default()
            };
            client.delete_subscription(request).await?;
            Ok(Answer { human: format!("deleted {subscription}\n"), json: Value::Null })
        }
        EventAction::Follow { .. } => {
            Err(CliError::Local("follow streams; it is answered by `follow`".into()))
        }
    }
}

/// Follows a subscription, handing each item to `out` as a line and as the API's JSON, and
/// acknowledging each event after handing it over when `ack` is set.
pub async fn follow(
    c: &Connection,
    subscription: &str,
    ack: bool,
    out: &mut dyn FnMut(String, Value),
) -> Result<(), CliError> {
    let client = pb::EventServiceClient::new(c.http(), c.config());
    let request =
        pb::SubscribeRequest { subscription: subscription.to_owned(), ..Default::default() };
    let mut stream = client.subscribe(request).await?;
    while let Some(m) = stream.message().await? {
        let Some(item) = m.to_owned_message().item else { continue };
        let value = match &item {
            Item::Event(e) => json(e.as_ref()),
            Item::Gap(g) => json(g.as_ref()),
        };
        out(line(&item), value);
        if let (true, Item::Event(e)) = (ack, &item) {
            let request = pb::AckRequest {
                subscription: subscription.to_owned(),
                through: e.id.clone(),
                ..Default::default()
            };
            client.ack(request).await?;
        }
    }
    Ok(())
}
