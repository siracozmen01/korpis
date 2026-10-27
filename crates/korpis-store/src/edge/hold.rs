// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases on the addresses of edge pools, and each member's health (migration 0133, §3.1 of
//! docs/07-networking.md). An address's row is written when its first epoch is issued and kept
//! after, so the next member's request is decided against the last holder's.

use super::rows::millis;
use crate::address::address;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{EdgeLeaseRead, EdgeLeaseWrite, StoreError, StoreResult};
use korpis_core::edge::lease::{Hold, MemberState};
use korpis_core::edge::probe::{AddressStatus, EdgeRoute};
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

struct HoldRow {
    address: String,
    group_id: Uuid,
    node_id: Uuid,
    epoch: i64,
    term_ms: i32,
    received_at: chrono::DateTime<chrono::Utc>,
    released_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn hold(r: &HoldRow) -> StoreResult<Hold> {
    let corrupt = || StoreError::Corrupt(format!("the lease on {}", r.address));
    let at = address(&r.address)?;
    Ok(Hold {
        address: at,
        group: r.group_id,
        node: r.node_id,
        epoch: u64::try_from(r.epoch).map_err(|_| corrupt())?,
        term: Duration::from_millis(u64::try_from(r.term_ms).map_err(|_| corrupt())?),
        received_at: to_time(r.received_at),
        released_at: r.released_at.map(to_time),
    })
}

struct StatusRow {
    address: String,
    group_id: Uuid,
    node_id: Uuid,
    epoch: i64,
    term_ms: i32,
    received_at: chrono::DateTime<chrono::Utc>,
    released_at: Option<chrono::DateTime<chrono::Utc>>,
    answering: Option<bool>,
    probed_at: Option<chrono::DateTime<chrono::Utc>>,
    router: Option<String>,
    routed_node_id: Option<Uuid>,
    route_reason: Option<String>,
}

fn status(r: &StatusRow) -> StoreResult<AddressStatus> {
    let row = HoldRow {
        address: r.address.clone(),
        group_id: r.group_id,
        node_id: r.node_id,
        epoch: r.epoch,
        term_ms: r.term_ms,
        received_at: r.received_at,
        released_at: r.released_at,
    };
    Ok(AddressStatus {
        hold: hold(&row)?,
        answering: r.answering,
        probed_at: r.probed_at.map(to_time),
        route: r.router.clone().map(|router| EdgeRoute {
            router,
            routed: r.routed_node_id,
            failing: r.route_reason.clone(),
        }),
    })
}

struct MemberRow {
    group_id: Uuid,
    node_id: Uuid,
    connected: Option<bool>,
    ready: Option<bool>,
    probed_at: Option<chrono::DateTime<chrono::Utc>>,
    reason: Option<String>,
}

fn member(r: MemberRow) -> MemberState {
    MemberState {
        group: r.group_id,
        node: r.node_id,
        connected: r.connected.unwrap_or(false),
        ready: r.ready,
        probed_at: r.probed_at.map(to_time),
        reason: r.reason,
    }
}

impl EdgeLeaseRead for PgStore {
    async fn edge_holds_all(&self) -> StoreResult<Vec<Hold>> {
        let rows = sqlx::query_as!(
            HoldRow,
            r#"SELECT host(address) AS "address!", group_id, node_id, epoch, term_ms, received_at,
                      released_at
                 FROM edge_hold ORDER BY address"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.iter().map(hold).collect()
    }

    async fn edge_members_all(&self) -> StoreResult<Vec<MemberState>> {
        let rows = sqlx::query_as!(
            MemberRow,
            r#"SELECT m.group_id, m.node_id, n.connectivity = 'connected' AS connected, m.ready,
                      m.probed_at, m.reason
                 FROM edge_member m JOIN node n ON n.id = m.node_id
                ORDER BY m.group_id, m.node_id"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(member).collect())
    }

    async fn edge_addresses_of(&self, group: Uuid) -> StoreResult<Vec<AddressStatus>> {
        let rows = sqlx::query_as!(
            StatusRow,
            r#"SELECT host(h.address) AS "address!", h.group_id, h.node_id, h.epoch, h.term_ms,
                      h.received_at, h.released_at, h.answering, h.probed_at,
                      p.router AS "router?", h.routed_node_id, h.route_reason
                 FROM edge_hold h
                 LEFT JOIN LATERAL (
                       SELECT router FROM address_pool
                        WHERE group_id = h.group_id AND cidr >>= h.address
                        LIMIT 1) p ON true
                WHERE h.group_id = $1 ORDER BY h.address"#,
            group
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.iter().map(status).collect()
    }

    async fn colocated_nodes(&self, now: SystemTime) -> StoreResult<BTreeSet<Uuid>> {
        super::colocated::nodes(&self.pool, now).await
    }
}

impl EdgeLeaseWrite for PgTx {
    async fn record_colocated(
        &mut self,
        replica: Uuid,
        nodes: &[Uuid],
        until: SystemTime,
    ) -> StoreResult<()> {
        super::colocated::record(self, replica, nodes, until).await
    }

    async fn claim_edge_probes(
        &mut self,
        replica: Uuid,
        (now, until): (SystemTime, SystemTime),
    ) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "UPDATE edge_group SET probe_holder = $1, probe_held_until = $3
              WHERE probe_holder = $1 OR probe_held_until IS NULL OR probe_held_until <= $2
              RETURNING id",
            replica,
            from_time(now),
            from_time(until)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)
    }

    async fn record_member_probe(
        &mut self,
        (group, node): (Uuid, Uuid),
        (passes, reason): (bool, Option<&str>),
        at: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE edge_member SET ready = $3, reason = $4, probed_at = $5
              WHERE group_id = $1 AND node_id = $2",
            group,
            node,
            passes,
            reason,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn record_address_probe(
        &mut self,
        at: IpAddr,
        answering: bool,
        when: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE edge_hold SET answering = $2, probed_at = $3 WHERE address = $1::text::inet",
            at.to_string(),
            answering,
            from_time(when)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn lock_edge_group_of(&mut self, at: IpAddr) -> StoreResult<Option<(Uuid, Duration)>> {
        let row = sqlx::query!(
            r#"SELECT g.id, g.term_ms FROM edge_group g
                 JOIN address_pool p ON p.group_id = g.id
                WHERE p.cidr >>= $1::text::inet
                  AND (EXISTS (SELECT 1 FROM stable_port s
                                WHERE s.address = $1::text::inet AND s.workload_id IS NOT NULL)
                       OR EXISTS (SELECT 1 FROM edge_ingress i
                                   WHERE i.address = $1::text::inet))
                  FOR UPDATE OF g"#,
            at.to_string()
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let Some(row) = row else { return Ok(None) };
        let term = u64::try_from(row.term_ms)
            .map_err(|_| StoreError::Corrupt(format!("edge group {}'s term", row.id)))?;
        Ok(Some((row.id, Duration::from_millis(term))))
    }

    async fn edge_members_now(&mut self, group: Uuid) -> StoreResult<Vec<MemberState>> {
        let rows = sqlx::query_as!(
            MemberRow,
            r#"SELECT m.group_id, m.node_id, n.connectivity = 'connected' AS connected, m.ready,
                      m.probed_at, m.reason
                 FROM edge_member m JOIN node n ON n.id = m.node_id
                WHERE m.group_id = $1 ORDER BY m.node_id"#,
            group
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(member).collect())
    }

    async fn edge_hold_for_update(&mut self, at: IpAddr) -> StoreResult<Option<Hold>> {
        let row = sqlx::query_as!(
            HoldRow,
            r#"SELECT host(address) AS "address!", group_id, node_id, epoch, term_ms, received_at,
                      released_at
                 FROM edge_hold WHERE address = $1::text::inet FOR UPDATE"#,
            at.to_string()
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.as_ref().map(hold).transpose()
    }

    async fn put_edge_hold(&mut self, h: &Hold) -> StoreResult<()> {
        let epoch = i64::try_from(h.epoch)
            .map_err(|_| StoreError::Corrupt(format!("epoch {} out of range", h.epoch)))?;
        sqlx::query!(
            r#"INSERT INTO edge_hold
                   (address, group_id, node_id, epoch, term_ms, received_at, released_at)
               VALUES ($1::text::inet, $2, $3, $4, $5, $6, $7)
               ON CONFLICT (address) DO UPDATE SET
                   group_id = $2, node_id = $3, epoch = $4, term_ms = $5, received_at = $6,
                   released_at = $7,
                   route_reason = CASE WHEN edge_hold.node_id = $3
                                       THEN edge_hold.route_reason END,
                   route_attempts = CASE WHEN edge_hold.node_id = $3
                                         THEN edge_hold.route_attempts ELSE 0 END,
                   route_due = CASE WHEN edge_hold.node_id = $3 THEN edge_hold.route_due END"#,
            h.address.to_string(),
            h.group,
            h.node,
            epoch,
            millis(h.term),
            from_time(h.received_at),
            h.released_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn renew_edge_hold(
        &mut self,
        at: IpAddr,
        (now, term): (SystemTime, Duration),
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE edge_hold SET received_at = $2, term_ms = $3 WHERE address = $1::text::inet",
            at.to_string(),
            from_time(now),
            millis(term)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn release_edge_hold(
        &mut self,
        (at, node, epoch): (IpAddr, Uuid, u64),
        now: SystemTime,
    ) -> StoreResult<bool> {
        let Ok(epoch) = i64::try_from(epoch) else { return Ok(false) };
        let done = sqlx::query!(
            r#"UPDATE edge_hold SET released_at = $4
                WHERE address = $1::text::inet AND node_id = $2 AND epoch = $3
                  AND released_at IS NULL"#,
            at.to_string(),
            node,
            epoch,
            from_time(now)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
