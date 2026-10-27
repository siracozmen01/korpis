// SPDX-License-Identifier: AGPL-3.0-or-later

//! The routes of routed edge pools' addresses (migration 0134, §3.1 of docs/07-networking.md).
//! An address is wanted at the member its lease was last issued to while that member has not
//! released it and a `stable` port holds the address or it is its group's ingress address, and
//! nowhere otherwise; a lease whose term
//! has passed unrenewed moves nothing by itself, since the member that holds the address next is
//! asked for at its grant.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::StoreResult;
use korpis_core::address::{DueRoute, Routed};
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

/// The first routed edge pool's address due, taken and held until `until`.
pub(super) async fn take_due(
    tx: &mut PgTx,
    (now, until): (SystemTime, SystemTime),
) -> StoreResult<Option<DueRoute>> {
    let row = sqlx::query!(
        r#"WITH due AS (
               SELECT h.address, p.organization_id, p.router, w.wanted
                 FROM edge_hold h
                 JOIN address_pool p
                   ON p.group_id = h.group_id AND p.cidr >>= h.address AND p.router IS NOT NULL
                CROSS JOIN LATERAL (
                       SELECT CASE WHEN h.released_at IS NULL AND (EXISTS (
                                       SELECT 1 FROM stable_port s
                                        WHERE s.address = h.address
                                          AND s.workload_id IS NOT NULL)
                                     OR EXISTS (SELECT 1 FROM edge_ingress i
                                                 WHERE i.address = h.address))
                                   THEN h.node_id END AS wanted) w
                WHERE w.wanted IS DISTINCT FROM h.routed_node_id
                  AND (h.route_due IS NULL OR h.route_due <= $1)
                  AND (h.route_held_until IS NULL OR h.route_held_until <= $1)
                ORDER BY h.address
                LIMIT 1
                  FOR UPDATE OF h SKIP LOCKED)
           UPDATE edge_hold h SET route_held_until = $2
             FROM due WHERE h.address = due.address
           RETURNING host(h.address) AS "address!", due.organization_id,
                     due.router AS "router!", due.wanted AS "wanted?", h.route_attempts"#,
        from_time(now),
        from_time(until)
    )
    .fetch_optional(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(DueRoute {
        of: Routed::Edge,
        address: crate::address::address(&r.address)?,
        organization: r.organization_id,
        router: r.router,
        wanted: r.wanted,
        attempts: u32::try_from(r.route_attempts).unwrap_or(0),
    }))
}

/// Records that the router routes `address` to `node`, or nowhere, and lets it go.
pub(super) async fn answered(
    tx: &mut PgTx,
    address: IpAddr,
    node: Option<Uuid>,
) -> StoreResult<()> {
    sqlx::query!(
        "UPDATE edge_hold
            SET routed_node_id = $2, route_reason = NULL, route_attempts = 0, route_due = NULL,
                route_held_until = NULL
          WHERE address = $1::text::inet",
        address.to_string(),
        node
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

/// Records the router's failure to route `address`, to be asked again at `due`.
pub(super) async fn failed(
    tx: &mut PgTx,
    address: IpAddr,
    (reason, attempts): (&str, u32),
    due: SystemTime,
) -> StoreResult<()> {
    sqlx::query!(
        "UPDATE edge_hold
            SET route_reason = $2, route_attempts = $3, route_due = $4, route_held_until = NULL
          WHERE address = $1::text::inet",
        address.to_string(),
        reason,
        i32::try_from(attempts).unwrap_or(i32::MAX),
        from_time(due)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}
