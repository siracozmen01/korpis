// SPDX-License-Identifier: AGPL-3.0-or-later

//! Publishing DNS records through a DNS provider (§3.5 of docs/07-networking.md). The records
//! are checked here, so a provider is only ever asked to publish what is well formed, and the
//! call is an ordinary provider call: queued, deadline-bound, and judged by the breaker.

use crate::App;
use crate::error::AppError;
use crate::extension::{CallTarget, Called};
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::dns::{DnsRecord, check_records};
use korpis_core::extension::{CallRequest, ProviderType};
use std::time::Duration;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Makes `owner`'s records exactly `records` through the DNS provider `target` names, waiting
    /// at most `timeout`. Internal: the use case publishing them decides who may.
    pub async fn apply_dns_records(
        &self,
        target: &CallTarget,
        owner: &str,
        records: Vec<DnsRecord>,
        timeout: Duration,
    ) -> Result<Called, AppError> {
        if target.kind != ProviderType::Dns {
            return Err(AppError::invalid("provider", "a dns provider"));
        }
        check_records(owner, &records).map_err(|e| AppError::invalid(e.field, e.message))?;
        let request = CallRequest::ApplyRecords { owner: owner.to_owned(), records };
        self.call_provider(target, request, timeout).await
    }
}
