// SPDX-License-Identifier: AGPL-3.0-or-later

//! Asking a pool's router to route its addresses (migrations 0131 and 0134, §3.7 and §3.1 of
//! docs/07-networking.md). A cluster pool's address is due when its holder's authoritative
//! placement is on another node than its router last said it routes it to, or when no placed
//! workload holds it and it is still routed; a routed edge pool's, likewise of the member its
//! lease names (`edge`); and either only when its backoff has passed and no control plane holds
//! it. One is taken at a time, skipping those another transaction has locked, and held for a
//! while, so that one control plane asks.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::from_time;
use korpis_app::ports::{AddressRoute, StoreResult};
use korpis_core::address::{DueRoute, Routed};
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

mod edge;

/// The first cluster pool's address due, taken.
async fn take_dedicated(
    tx: &mut PgTx,
    (now, until): (SystemTime, SystemTime),
) -> StoreResult<Option<DueRoute>> {
    let row = sqlx::query!(
        r#"WITH due AS (
                   SELECT d.address, p.organization_id, p.router, pl.node_id AS wanted
                     FROM dedicated_address d
                     JOIN address_pool p ON p.id = d.pool_id AND p.scope = 'cluster'
                     LEFT JOIN placement pl
                       ON pl.workload_id = d.workload_id AND pl.authoritative
                    WHERE pl.node_id IS DISTINCT FROM d.routed_node_id
                      AND (d.route_due IS NULL OR d.route_due <= $1)
                      AND (d.route_held_until IS NULL OR d.route_held_until <= $1)
                    ORDER BY d.address
                    LIMIT 1
                      FOR UPDATE OF d SKIP LOCKED)
               UPDATE dedicated_address d SET route_held_until = $2
                 FROM due WHERE d.address = due.address
               RETURNING host(d.address) AS "address!", due.organization_id,
                         due.router AS "router!", due.wanted AS "wanted?", d.route_attempts"#,
        from_time(now),
        from_time(until)
    )
    .fetch_optional(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    let Some(r) = row else { return Ok(None) };
    Ok(Some(DueRoute {
        of: Routed::Dedicated,
        address: super::address(&r.address)?,
        organization: r.organization_id,
        router: r.router,
        wanted: r.wanted,
        attempts: u32::try_from(r.route_attempts).unwrap_or(0),
    }))
}

impl AddressRoute for PgTx {
    async fn take_due_route(
        &mut self,
        now: SystemTime,
        until: SystemTime,
    ) -> StoreResult<Option<DueRoute>> {
        match take_dedicated(self, (now, until)).await? {
            Some(due) => Ok(Some(due)),
            None => edge::take_due(self, (now, until)).await,
        }
    }

    async fn route_answered(
        &mut self,
        (of, address): (Routed, IpAddr),
        node: Option<Uuid>,
    ) -> StoreResult<()> {
        if of == Routed::Edge {
            return edge::answered(self, address, node).await;
        }
        sqlx::query!(
            "UPDATE dedicated_address
                SET routed_node_id = $2, route_reason = NULL, route_attempts = 0,
                    route_due = NULL, route_held_until = NULL
              WHERE address = $1::text::inet",
            address.to_string(),
            node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn route_failed(
        &mut self,
        (of, address): (Routed, IpAddr),
        failure: (&str, u32),
        due: SystemTime,
    ) -> StoreResult<()> {
        if of == Routed::Edge {
            return edge::failed(self, address, failure, due).await;
        }
        let (reason, attempts) = failure;
        sqlx::query!(
            "UPDATE dedicated_address
                SET route_reason = $2, route_attempts = $3, route_due = $4,
                    route_held_until = NULL
              WHERE address = $1::text::inet",
            address.to_string(),
            reason,
            i32::try_from(attempts).unwrap_or(i32::MAX),
            from_time(due)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
