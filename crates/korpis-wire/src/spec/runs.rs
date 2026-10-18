// SPDX-License-Identifier: AGPL-3.0-or-later

//! A run policy: `korpis.v1.RunPolicy` (§4 of docs/05-scheduling.md). A field left unset is the
//! default, so the wire's absent and the policy's default are one value.

use crate::{Result, duration, duration_from};
use korpis_core::workload::spec::runs::ALLOW_ACTIVE;
use korpis_core::workload::spec::{Concurrency, RunPolicy};
use korpis_proto::korpis::v1 as pb;

enum_map!(concurrency_to_pb, concurrency_known, Concurrency, pb::Concurrency, "a concurrency", {
    Concurrency::Forbid => pb::Concurrency::CONCURRENCY_FORBID,
    Concurrency::Replace => pb::Concurrency::CONCURRENCY_REPLACE,
    Concurrency::Allow => pb::Concurrency::CONCURRENCY_ALLOW,
});

/// The wire form of a run policy, every field set.
#[must_use]
pub fn policy_to_pb(p: &RunPolicy) -> pb::RunPolicy {
    pb::RunPolicy {
        concurrency: concurrency_to_pb(p.concurrency).into(),
        retries: Some(p.retries),
        backoff: duration(p.backoff).into(),
        timeout: p.timeout.map(duration).into(),
        keep_successful: Some(p.keep_successful),
        keep_failed: Some(p.keep_failed),
        catch_up: p.catch_up,
        max_active: Some(p.max_active),
        ..Default::default()
    }
}

/// A run policy from the field `field`, each unset part the default.
pub fn policy_from_pb(field: &str, p: &pb::RunPolicy) -> Result<RunPolicy> {
    let default = RunPolicy::default();
    let f = |name: &str| format!("{field}.{name}");
    let concurrency = match p.concurrency.as_known() {
        Some(pb::Concurrency::CONCURRENCY_UNSPECIFIED) => default.concurrency,
        _ => concurrency_known(&f("concurrency"), p.concurrency)?,
    };
    let backoff = match p.backoff.as_option() {
        None => default.backoff,
        Some(d) => duration_from(&f("backoff"), d)?,
    };
    let timeout = p.timeout.as_option().map(|d| duration_from(&f("timeout"), d)).transpose()?;
    Ok(RunPolicy {
        concurrency,
        retries: p.retries.unwrap_or(default.retries),
        backoff,
        timeout,
        keep_successful: p.keep_successful.unwrap_or(default.keep_successful),
        keep_failed: p.keep_failed.unwrap_or(default.keep_failed),
        catch_up: p.catch_up,
        // Unset, it is what the concurrency keeps active: one, or under `allow` its default.
        max_active: p.max_active.unwrap_or(match concurrency {
            Concurrency::Allow => ALLOW_ACTIVE,
            Concurrency::Forbid | Concurrency::Replace => default.max_active,
        }),
    })
}
