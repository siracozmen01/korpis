// SPDX-License-Identifier: AGPL-3.0-or-later

//! An organization's status as it is kept: `status` says whether it is suspended, and
//! `suspension` who suspended it, when, why, and what it did to the workloads (§2.1 of
//! docs/08-identity.md). The two are written together and read together, so a row never says one
//! without the other.

use crate::stored::{from_json, principal, to_json};
use chrono::{DateTime, Utc};
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::tenancy::{OnSuspend, OrganizationStatus, Suspension};
use serde::{Deserialize, Serialize};

/// A suspension in the `suspension` column.
#[derive(Debug, Serialize, Deserialize)]
struct SuspensionDoc {
    by: String,
    at: DateTime<Utc>,
    reason: String,
    on_suspend: String,
}

/// The `status` and `suspension` columns of `status`.
pub(crate) fn encode(
    status: &OrganizationStatus,
) -> StoreResult<(&'static str, Option<serde_json::Value>)> {
    Ok(match status {
        OrganizationStatus::Active => ("active", None),
        OrganizationStatus::Suspended(s) => {
            let doc = SuspensionDoc {
                by: s.by.key(),
                at: DateTime::<Utc>::from(s.at),
                reason: s.reason.clone(),
                on_suspend: match s.on_suspend {
                    OnSuspend::KeepRunning => "keep_running",
                    OnSuspend::Stop => "stop",
                }
                .to_owned(),
            };
            ("suspended", Some(to_json("suspension", &doc)?))
        }
    })
}

/// The status the `status` and `suspension` columns hold.
pub(crate) fn decode(
    status: &str,
    suspension: Option<serde_json::Value>,
) -> StoreResult<OrganizationStatus> {
    match (status, suspension) {
        ("active", _) => Ok(OrganizationStatus::Active),
        ("suspended", Some(v)) => {
            let doc: SuspensionDoc = from_json("suspension", v)?;
            let on_suspend = match doc.on_suspend.as_str() {
                "keep_running" => OnSuspend::KeepRunning,
                "stop" => OnSuspend::Stop,
                other => return Err(StoreError::Corrupt(format!("on_suspend {other:?}"))),
            };
            Ok(OrganizationStatus::Suspended(Suspension {
                by: principal(&doc.by)?,
                at: doc.at.into(),
                reason: doc.reason,
                on_suspend,
            }))
        }
        (other, _) => Err(StoreError::Corrupt(format!("organization status {other:?}"))),
    }
}
