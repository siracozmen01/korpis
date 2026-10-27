// SPDX-License-Identifier: AGPL-3.0-or-later

//! The names of ingress endpoints in DNS (migration 0063): synchronized with the endpoints the
//! ingress serves in a few statements, and taken one at a time under a lease, as the issuer takes
//! a certificate.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::dns::published::{EndpointRecord, RecordState};
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

struct Row {
    endpoint_id: Uuid,
    name: String,
    workload_id: Uuid,
    organization_id: Uuid,
    owner: String,
    wanted: Option<Vec<String>>,
    applied: Option<Vec<String>>,
    provider: Option<String>,
    state: String,
    reason: Option<String>,
    attempts: i32,
    next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn addresses(of: Option<Vec<String>>, what: &str) -> StoreResult<Option<Vec<IpAddr>>> {
    of.map(|a| {
        a.iter()
            .map(|s| s.parse().map_err(|_| StoreError::Corrupt(format!("{what} {s:?}"))))
            .collect()
    })
    .transpose()
}

fn texts(of: Option<&Vec<IpAddr>>) -> Option<Vec<String>> {
    of.map(|a| a.iter().map(ToString::to_string).collect())
}

impl TryFrom<Row> for EndpointRecord {
    type Error = StoreError;

    fn try_from(r: Row) -> StoreResult<Self> {
        let state = RecordState::parse(&r.state)
            .ok_or_else(|| StoreError::Corrupt(format!("endpoint record state {:?}", r.state)))?;
        Ok(Self {
            endpoint: r.endpoint_id,
            name: r.name,
            workload: r.workload_id,
            organization: r.organization_id,
            owner: r.owner,
            wanted: addresses(r.wanted, "wanted address")?,
            applied: addresses(r.applied, "applied address")?,
            provider: r.provider,
            state,
            reason: r.reason,
            attempts: u32::try_from(r.attempts).unwrap_or(0),
            next_attempt: r.next_attempt_at.map(to_time),
        })
    }
}

pub(super) async fn endpoint_records(
    store: &PgStore,
    workload: Uuid,
) -> StoreResult<Vec<EndpointRecord>> {
    let rows = sqlx::query_as!(
        Row,
        "SELECT endpoint_id, name, workload_id, organization_id, owner, wanted, applied, provider,
                state, reason, attempts, next_attempt_at
           FROM endpoint_record WHERE workload_id = $1 AND wanted IS NOT NULL
          ORDER BY name",
        workload
    )
    .fetch_all(&store.pool)
    .await
    .map_err(from_sqlx)?;
    rows.into_iter().map(EndpointRecord::try_from).collect()
}

pub(super) async fn sync(tx: &mut PgTx, now: SystemTime) -> StoreResult<()> {
    sqlx::query!(
        "INSERT INTO endpoint_record (endpoint_id, name, workload_id, organization_id, owner,
                                      wanted, state)
         SELECT e.id, d.name, e.workload_id, w.organization_id,
                'ep_' || replace(e.id::text, '-', '') || '/'
                    || left(encode(sha256(convert_to(d.name, 'UTF8')), 'hex'), 16),
                COALESCE(NULLIF(ARRAY(SELECT host(i.address) FROM ingress_served s
                                        JOIN edge_ingress i ON i.group_id = s.group_id
                                       WHERE s.workload_id = e.workload_id
                                       ORDER BY family(i.address)), '{}'),
                         n.addresses),
                'pending'
           FROM endpoint e
           JOIN workload w ON w.id = e.workload_id
           JOIN node n ON n.id = e.node_id
          CROSS JOIN LATERAL unnest(e.domains) AS d(name)
          WHERE e.exposure = 'ingress'
         ON CONFLICT (endpoint_id, name) DO UPDATE
            SET wanted = EXCLUDED.wanted, next_attempt_at = NULL
          WHERE endpoint_record.wanted IS DISTINCT FROM EXCLUDED.wanted"
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    sqlx::query!(
        "UPDATE endpoint_record r SET wanted = NULL, next_attempt_at = NULL
          WHERE r.wanted IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM endpoint e
                             WHERE e.id = r.endpoint_id AND e.exposure = 'ingress'
                               AND r.name = ANY (e.domains))"
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    sqlx::query!(
        "DELETE FROM endpoint_record
          WHERE wanted IS NULL AND provider IS NULL
            AND (applying_until IS NULL OR applying_until < $1)",
        from_time(now)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn take_due(
    tx: &mut PgTx,
    now: SystemTime,
    until: SystemTime,
) -> StoreResult<Option<EndpointRecord>> {
    let row = sqlx::query_as!(
        Row,
        "UPDATE endpoint_record SET applying_until = $2
          WHERE (endpoint_id, name) = (
                SELECT endpoint_id, name FROM endpoint_record
                 WHERE (applying_until IS NULL OR applying_until < $1)
                   AND (next_attempt_at IS NULL OR next_attempt_at <= $1)
                   AND (state IN ('pending', 'failed', 'unmanaged')
                        OR wanted IS DISTINCT FROM applied)
                 ORDER BY next_attempt_at NULLS FIRST, endpoint_id, name
                 LIMIT 1 FOR UPDATE SKIP LOCKED)
          RETURNING endpoint_id, name, workload_id, organization_id, owner, wanted, applied,
                    provider, state, reason, attempts, next_attempt_at",
        from_time(now),
        from_time(until)
    )
    .fetch_optional(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    row.map(EndpointRecord::try_from).transpose()
}

pub(super) async fn put(tx: &mut PgTx, r: &EndpointRecord) -> StoreResult<()> {
    let applied = texts(r.applied.as_ref());
    sqlx::query!(
        "UPDATE endpoint_record
            SET applied = $3, provider = $4, state = $5, reason = $6, attempts = $7,
                next_attempt_at = $8, applying_until = NULL
          WHERE endpoint_id = $1 AND name = $2",
        r.endpoint,
        r.name,
        applied.as_deref(),
        r.provider,
        r.state.as_str(),
        r.reason,
        i32::try_from(r.attempts).unwrap_or(i32::MAX),
        r.next_attempt.map(from_time)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(super) async fn drop(tx: &mut PgTx, endpoint: Uuid, name: &str) -> StoreResult<()> {
    sqlx::query!(
        "DELETE FROM endpoint_record WHERE endpoint_id = $1 AND name = $2",
        endpoint,
        name
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}
