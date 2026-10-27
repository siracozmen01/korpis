// SPDX-License-Identifier: AGPL-3.0-or-later

//! Publishing the names of ingress endpoints (§3.5 of docs/07-networking.md). Each name an
//! endpoint the ingress serves routes resolves to its node's advertised addresses, or, where an
//! edge group serves it, to the group's ingress addresses (§3.1 there), published
//! through a DNS provider of the endpoint's organization: the one that published it before, then
//! the others it sees, its own before its ancestors', until one serves the name's zone. A name no
//! provider serves is its holder's to publish, and its status says so; the providers are asked
//! again hourly, one installed meanwhile among them. A name no longer routed is withdrawn from
//! the provider that published it. Only names an endpoint routes are published, and a name is
//! routed only by the organization holding it (§3.2), so the control plane has checked the
//! holder before any provider is asked.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Clock, EndpointWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::dns::certificate::backoff;
use korpis_core::dns::published::{
    EndpointRecord, RecordState, UNMANAGED_RECHECK, address_records,
};
use korpis_core::extension::{CallOutcome, ProviderType};
use std::time::Duration;

/// How long a reconciler holds the name it took.
const HOLD: Duration = Duration::from_secs(5 * 60);
/// How long a DNS provider is given to answer.
const CALL_WITHIN: Duration = Duration::from_secs(30);
/// What a DNS provider answers for a name in no zone it manages.
const ZONE_NOT_FOUND: &str = "dns.zone_not_found";
/// What a name no provider serves says.
const UNMANAGED: &str = "no DNS provider of its organization serves its zone: its holder \
                         publishes A and AAAA records for the addresses its endpoint shows";

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Makes the names in DNS follow the endpoints the ingress serves.
    pub async fn sync_endpoint_records(&self) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.sync_endpoint_records(self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Publishes or withdraws the next name due, if one is; whether one was.
    pub async fn publish_next_record(&self) -> Result<bool, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let due = tx.take_due_record(now, now + HOLD).await?;
        tx.commit().await?;
        let Some(r) = due else { return Ok(false) };
        match r.wanted.clone() {
            Some(addresses) => self.publish_record(r, addresses).await?,
            None => self.withdraw_record(r).await?,
        }
        Ok(true)
    }

    fn target(r: &EndpointRecord, provider: &str) -> CallTarget {
        CallTarget {
            organization: r.organization,
            kind: ProviderType::Dns,
            name: provider.to_owned(),
        }
    }

    async fn publish_record(
        &self,
        r: EndpointRecord,
        addresses: Vec<std::net::IpAddr>,
    ) -> Result<(), AppError> {
        let records = address_records(&r.name, &addresses);
        let mut providers = self.dns_providers(r.organization).await?;
        if let Some(p) = &r.provider {
            providers.retain(|q| q != p);
            providers.insert(0, p.clone());
        }
        for p in providers {
            let target = Self::target(&r, &p);
            let called = self.apply_dns_records(&target, &r.owner, records.clone(), CALL_WITHIN);
            let outcome = match called.await {
                Ok(c) => c.outcome,
                Err(e) => return self.record_failed(r, Some(p), &e.to_string()).await,
            };
            match outcome {
                CallOutcome::RecordsApplied { .. } => {
                    let published = EndpointRecord {
                        applied: Some(addresses),
                        provider: Some(p),
                        state: RecordState::Published,
                        reason: None,
                        attempts: 0,
                        next_attempt: None,
                        ..r
                    };
                    return self.put_record(&published).await;
                }
                CallOutcome::Failed(f) if f.reason == ZONE_NOT_FOUND => {}
                CallOutcome::Failed(f) => {
                    let why = format!("{}: {}", f.reason, f.message);
                    return self.record_failed(r, Some(p), &why).await;
                }
                _ => return self.record_failed(r, Some(p), "an answer that answers nothing").await,
            }
        }
        let unmanaged = EndpointRecord {
            applied: None,
            provider: None,
            state: RecordState::Unmanaged,
            reason: Some(UNMANAGED.into()),
            attempts: 0,
            next_attempt: Some(self.clock.now() + UNMANAGED_RECHECK),
            ..r
        };
        self.put_record(&unmanaged).await
    }

    async fn withdraw_record(&self, r: EndpointRecord) -> Result<(), AppError> {
        let gone = match &r.provider {
            None => true,
            Some(p) => {
                let target = Self::target(&r, p);
                match self.apply_dns_records(&target, &r.owner, Vec::new(), CALL_WITHIN).await {
                    Ok(c) => match c.outcome {
                        CallOutcome::RecordsApplied { .. } => true,
                        CallOutcome::Failed(f) => !f.retryable,
                        _ => false,
                    },
                    Err(_) => false,
                }
            }
        };
        if !gone {
            let provider = r.provider.clone();
            return self.record_failed(r, provider, "withdrawing it failed").await;
        }
        let mut tx = self.store.begin().await?;
        tx.drop_endpoint_record(r.endpoint, &r.name).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn record_failed(
        &self,
        r: EndpointRecord,
        provider: Option<String>,
        why: &str,
    ) -> Result<(), AppError> {
        let attempts = r.attempts.saturating_add(1);
        let failed = EndpointRecord {
            provider,
            state: RecordState::Failed,
            reason: Some(why.to_owned()),
            attempts,
            next_attempt: Some(self.clock.now() + backoff(attempts)),
            ..r
        };
        self.put_record(&failed).await
    }

    async fn put_record(&self, r: &EndpointRecord) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.put_endpoint_record(r).await?;
        tx.commit().await?;
        Ok(())
    }
}
