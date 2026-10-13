// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis extension providers|ping` (§7 of docs/13-surface-cli.md, §3.3 of
//! docs/16-extensions.md): the provider streams registered in an organization, and a liveness
//! call to one, reported however it ended.

use super::{Answer, json};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use korpis_core::extension::ProviderType;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

fn rows(list: &[pb::ProviderRegistration]) -> String {
    let mut all = vec![
        ["ID", "TYPE", "NAME", "VERSION", "CONCURRENCY", "EXTENSION", "CONNECTED", "SEEN"]
            .map(str::to_owned)
            .to_vec(),
    ];
    all.extend(list.iter().map(|r| {
        vec![
            r.id.clone(),
            short(r.r#type, "PROVIDER_TYPE_").to_lowercase(),
            r.name.clone(),
            r.version.clone(),
            r.concurrency.to_string(),
            r.extension.clone(),
            time(r.connected.as_option()),
            time(r.seen.as_option()),
        ]
    }));
    table(&all)
}

/// Answers `korpis extension providers`.
pub async fn providers(c: &Connection, organization: Option<&str>) -> Result<Answer, CliError> {
    let client = pb::ProviderServiceClient::new(c.http(), c.config());
    let organization = resolve::organization(c, organization).await?;
    let request = pb::ListProvidersRequest { organization, ..Default::default() };
    let list = client.list_providers(request).await?.into_owned().providers;
    Ok(Answer { human: rows(&list), json: json(&list) })
}

/// Answers `korpis extension ping`.
pub async fn ping(
    c: &Connection,
    kind: &str,
    name: &str,
    organization: Option<&str>,
    timeout: Duration,
) -> Result<Answer, CliError> {
    let kind = ProviderType::parse(kind).ok_or_else(|| {
        CliError::Local(format!("{kind:?}: a provider type, such as dns or backup_target"))
    })?;
    let client = pb::ProviderServiceClient::new(c.http(), c.config());
    let request = pb::PingProviderRequest {
        organization: resolve::organization(c, organization).await?,
        r#type: korpis_wire::extension::provider_type_to_pb(kind).into(),
        name: name.to_owned(),
        timeout: korpis_wire::duration(timeout).into(),
        ..Default::default()
    };
    let r = client.ping_provider(request).await?.into_owned();
    let elapsed =
        r.elapsed.as_option().map_or(0, |d| d.seconds * 1000 + i64::from(d.nanos) / 1_000_000);
    let human = match r.error.as_option() {
        Some(e) => {
            let again = if e.retryable { "; retrying may help" } else { "" };
            format!("{} after {elapsed} ms: {}{again}\n", e.reason, e.message)
        }
        None => format!("answered by {} ({}) in {elapsed} ms\n", r.registration, r.version),
    };
    Ok(Answer { human, json: json(&r) })
}
