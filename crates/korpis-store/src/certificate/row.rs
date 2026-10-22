// SPDX-License-Identifier: AGPL-3.0-or-later

//! A certificate as its row holds it.

use crate::stored::to_time;
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::dns::certificate::{Certificate, Owner, Source, State};
use uuid::Uuid;

/// A row of `certificate`, as every query reading one selects it.
pub(crate) struct CertificateRow {
    pub(crate) id: Uuid,
    pub(crate) organization_id: Uuid,
    pub(crate) names: Vec<String>,
    pub(crate) source: String,
    pub(crate) chain: String,
    pub(crate) not_before: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) not_after: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) state: String,
    pub(crate) reason: Option<String>,
    pub(crate) renew_after: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) revision: i64,
    pub(crate) created_at: chrono::DateTime<chrono::Utc>,
    pub(crate) workload_id: Option<Uuid>,
    pub(crate) endpoint: Option<String>,
    pub(crate) portal_id: Option<Uuid>,
    pub(crate) attempts: i32,
    pub(crate) next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) key_id: Option<String>,
    pub(crate) exposed: bool,
}

impl TryFrom<CertificateRow> for Certificate {
    type Error = StoreError;

    fn try_from(r: CertificateRow) -> StoreResult<Self> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("certificate {}: {what}", r.id));
        Ok(Self {
            id: r.id,
            organization: r.organization_id,
            names: r.names,
            source: Source::parse(&r.source).ok_or_else(|| corrupt("source"))?,
            chain: r.chain,
            not_before: r.not_before.map(to_time),
            not_after: r.not_after.map(to_time),
            state: State::parse(&r.state).ok_or_else(|| corrupt("state"))?,
            reason: r.reason,
            renew_after: r.renew_after.map(to_time),
            revision: r.revision,
            created_at: to_time(r.created_at),
            owner: r
                .workload_id
                .zip(r.endpoint)
                .map(|(workload, endpoint)| Owner { workload, endpoint }),
            portal: r.portal_id,
            attempts: u32::try_from(r.attempts).map_err(|_| corrupt("attempts"))?,
            next_attempt: r.next_attempt_at.map(to_time),
            key_id: r.key_id,
            exposed: r.exposed,
        })
    }
}
