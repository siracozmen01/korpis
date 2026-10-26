// SPDX-License-Identifier: AGPL-3.0-or-later

//! Address pools and dedicated addresses (migration 0130, §3.7 of docs/07-networking.md). Two
//! pools never overlap, which an exclusion constraint holds, and an address is one workload's at
//! most, which its key does; a released address keeps its row, resting, until it is given again.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{AddressRead, AddressWrite, StoreError, StoreResult};
use korpis_core::address::{AddressPool, Holding, Routing, Scope, Taken};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

struct PoolRow {
    id: Uuid,
    organization_id: Uuid,
    cidr: Option<String>,
    scope: String,
    node_id: Option<Uuid>,
    router: Option<String>,
    group_id: Option<Uuid>,
    routing: String,
    interface: String,
    quarantine_secs: i64,
    reserved: Option<Vec<String>>,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<PoolRow> for AddressPool {
    type Error = StoreError;

    fn try_from(r: PoolRow) -> StoreResult<Self> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("address pool {}: {what}", r.id));
        let scope = match (r.scope.as_str(), r.node_id, r.router.clone(), r.group_id) {
            ("node", Some(n), _, _) => Scope::Node(n),
            ("edge", _, router, Some(group)) => Scope::Edge { group, router },
            ("cluster", _, Some(router), _) => Scope::Cluster(router),
            _ => return Err(corrupt("scope")),
        };
        let routing = match r.routing.as_str() {
            "static" => Routing::Static,
            "bgp" => Routing::Bgp,
            "provider_api" => Routing::ProviderApi,
            _ => return Err(corrupt("routing")),
        };
        // An `inet` in text carries its netmask, `/32` for a host's address.
        let host = |a: &String| a.split('/').next().unwrap_or_default().parse::<IpAddr>();
        let reserved: Result<Vec<_>, _> = r.reserved.unwrap_or_default().iter().map(host).collect();
        Ok(Self {
            id: r.id,
            organization: r.organization_id,
            cidr: r.cidr.as_deref().and_then(|c| c.parse().ok()).ok_or_else(|| corrupt("cidr"))?,
            scope,
            routing,
            interface: r.interface,
            quarantine: Duration::from_secs(u64::try_from(r.quarantine_secs).unwrap_or(0)),
            reserved: reserved.map_err(|_| corrupt("reserved"))?,
            created_at: to_time(r.created_at),
        })
    }
}

fn pools(rows: Vec<PoolRow>) -> StoreResult<Vec<AddressPool>> {
    rows.into_iter().map(AddressPool::try_from).collect()
}

pub(crate) fn address(text: &str) -> StoreResult<IpAddr> {
    text.parse().map_err(|_| StoreError::Corrupt(format!("dedicated address {text}")))
}

impl AddressRead for PgStore {
    async fn address_pool(&self, id: Uuid) -> StoreResult<Option<AddressPool>> {
        let row = sqlx::query_as!(
            PoolRow,
            r#"SELECT id, organization_id, cidr::text AS cidr, scope, node_id, router, group_id,
                      routing,
                      interface, quarantine_secs, reserved::text[] AS reserved, created_at
                 FROM address_pool WHERE id = $1"#,
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(AddressPool::try_from).transpose()
    }

    async fn address_pools_of(
        &self,
        organization: Uuid,
        page: Page,
    ) -> StoreResult<Vec<AddressPool>> {
        let rows = sqlx::query_as!(
            PoolRow,
            r#"SELECT id, organization_id, cidr::text AS cidr, scope, node_id, router, group_id,
                      routing,
                      interface, quarantine_secs, reserved::text[] AS reserved, created_at
                 FROM address_pool
                WHERE organization_id = $1 AND ($2::uuid IS NULL OR id > $2)
                ORDER BY id LIMIT $3"#,
            organization,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        pools(rows)
    }

    async fn dedicated_of(&self, workload: Uuid) -> StoreResult<Option<Holding>> {
        let row = sqlx::query!(
            r#"SELECT host(d.address) AS "address!", d.pool_id, p.scope, d.routed_node_id,
                      d.route_reason
                 FROM dedicated_address d JOIN address_pool p ON p.id = d.pool_id
                WHERE d.workload_id = $1"#,
            workload
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(Holding {
                address: address(&r.address)?,
                pool: r.pool_id,
                cluster: r.scope == "cluster",
                routed: r.routed_node_id,
                failing: r.route_reason,
            })
        })
        .transpose()
    }

    async fn pool_held(&self, pool: Uuid) -> StoreResult<u64> {
        // An edge pool's are its addresses and ports that `stable` endpoints hold.
        let n = sqlx::query_scalar!(
            r#"SELECT (SELECT count(*) FROM dedicated_address
                        WHERE pool_id = $1 AND workload_id IS NOT NULL)
                    + (SELECT count(*) FROM stable_port
                        WHERE pool_id = $1 AND workload_id IS NOT NULL) AS "n!""#,
            pool
        )
        .fetch_one(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(u64::try_from(n).unwrap_or(0))
    }
}

impl AddressWrite for PgTx {
    async fn insert_address_pool(&mut self, p: &AddressPool) -> StoreResult<()> {
        let (scope, node, group) = match &p.scope {
            Scope::Node(n) => ("node", Some(*n), None),
            Scope::Edge { group, .. } => ("edge", None, Some(*group)),
            Scope::Cluster(_) => ("cluster", None, None),
        };
        let router = p.router();
        let routing = match p.routing {
            Routing::Static => "static",
            Routing::Bgp => "bgp",
            Routing::ProviderApi => "provider_api",
        };
        let family = if p.cidr.addr().is_ipv4() { "ipv4" } else { "ipv6" };
        let reserved: Vec<String> = p.reserved.iter().map(ToString::to_string).collect();
        let quarantine = i64::try_from(p.quarantine.as_secs()).unwrap_or(i64::MAX);
        sqlx::query!(
            "INSERT INTO address_pool (id, organization_id, family, cidr, scope, node_id, router,
                                       group_id, routing, interface, quarantine_secs, reserved,
                                       created_at)
             VALUES ($1, $2, $3, $4::text::cidr, $5, $6, $7, $8, $9, $10, $11,
                     $12::text[]::inet[], $13)",
            p.id,
            p.organization,
            family,
            p.cidr.to_string(),
            scope,
            node,
            router,
            group,
            routing,
            p.interface,
            quarantine,
            &reserved,
            from_time(p.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn delete_address_pool(&mut self, id: Uuid) -> StoreResult<bool> {
        let done = sqlx::query!("DELETE FROM address_pool WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(done.rows_affected() > 0)
    }

    async fn address_pools_now(&mut self, organization: Uuid) -> StoreResult<Vec<AddressPool>> {
        let rows = sqlx::query_as!(
            PoolRow,
            r#"SELECT id, organization_id, cidr::text AS cidr, scope, node_id, router, group_id,
                      routing,
                      interface, quarantine_secs, reserved::text[] AS reserved, created_at
                 FROM address_pool WHERE organization_id = $1 ORDER BY id"#,
            organization
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        pools(rows)
    }

    async fn pool_taken(&mut self, pool: Uuid) -> StoreResult<BTreeMap<IpAddr, Taken>> {
        let rows = sqlx::query!(
            r#"SELECT host(address) AS "address!", workload_id, released_at FROM dedicated_address
                WHERE pool_id = $1"#,
            pool
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let taken = match (r.workload_id, r.released_at) {
                    (Some(w), _) => Taken::Held(w),
                    (None, Some(at)) => Taken::Released(to_time(at)),
                    (None, None) => {
                        return Err(StoreError::Corrupt(format!(
                            "dedicated address {}",
                            r.address
                        )));
                    }
                };
                Ok((address(&r.address)?, taken))
            })
            .collect()
    }

    async fn dedicated_now(&mut self, workload: Uuid) -> StoreResult<Option<(IpAddr, Uuid)>> {
        let row = sqlx::query!(
            r#"SELECT host(address) AS "address!", pool_id FROM dedicated_address
                WHERE workload_id = $1"#,
            workload
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| Ok((address(&r.address)?, r.pool_id))).transpose()
    }

    async fn hold_address(
        &mut self,
        (pool, address): (Uuid, IpAddr),
        workload: Uuid,
        at: SystemTime,
    ) -> StoreResult<()> {
        let done = sqlx::query!(
            "INSERT INTO dedicated_address (address, pool_id, workload_id, held_at, released_at)
             VALUES ($1::text::inet, $2, $3, $4, NULL)
             ON CONFLICT (address) DO UPDATE
                SET pool_id = $2, workload_id = $3, held_at = $4, released_at = NULL
              WHERE dedicated_address.workload_id IS NULL",
            address.to_string(),
            pool,
            workload,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        // Another workload holds it: placement chooses under its lock, so this is a fault.
        if done.rows_affected() == 0 {
            return Err(StoreError::Duplicate(korpis_app::ports::Unique::Other));
        }
        Ok(())
    }

    async fn release_address(&mut self, workload: Uuid, at: SystemTime) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE dedicated_address SET workload_id = NULL, released_at = $2
              WHERE workload_id = $1",
            workload,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() > 0)
    }
}

mod route;
