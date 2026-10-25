// SPDX-License-Identifier: AGPL-3.0-or-later

//! Each node's workload pool (§9.1 of docs/07-networking.md): the lowest free /22 of the
//! installation's range, chosen under a lock every enrollment takes, so two enrolling at once
//! are never given one.

use crate::errors::from_sqlx;
use crate::{PgStore, PgTx};
use ipnet::Ipv4Net;
use korpis_app::ports::{StoreError, StoreResult};
use korpis_core::node::pool::lowest_free;
use uuid::Uuid;

/// The lock every pool's choice takes, apart from every other lock's key.
const POOL_LOCK: i64 = 0x6b6f_7270_6973_0003;

/// `text` as a network, or the store's error naming it.
fn network(text: &str) -> StoreResult<Ipv4Net> {
    text.parse().map_err(|_| StoreError::Corrupt(format!("{text:?} is no IPv4 pool")))
}

impl PgStore {
    pub(crate) async fn pool_of(&self, node: Uuid) -> StoreResult<Option<Ipv4Net>> {
        let row = sqlx::query_scalar!(
            r#"SELECT ipv4::text AS "ipv4!" FROM node_pool WHERE node_id = $1"#,
            node
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.as_deref().map(network).transpose()
    }
}

impl PgTx {
    pub(crate) async fn take_pool_in(
        &mut self,
        node: Uuid,
        range: Ipv4Net,
    ) -> StoreResult<Option<Ipv4Net>> {
        sqlx::query!("SELECT pg_advisory_xact_lock($1)", POOL_LOCK)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let held = sqlx::query_scalar!(r#"SELECT ipv4::text AS "ipv4!" FROM node_pool"#)
            .fetch_all(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let taken = held.iter().map(|t| network(t)).collect::<StoreResult<Vec<_>>>()?;
        let Some(pool) = lowest_free(range, &taken) else { return Ok(None) };
        sqlx::query!(
            "INSERT INTO node_pool (node_id, ipv4) VALUES ($1, $2::text::cidr)",
            node,
            pool.to_string()
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(Some(pool))
    }

    pub(crate) async fn release_pool_in(&mut self, node: Uuid) -> StoreResult<bool> {
        let done = sqlx::query!("DELETE FROM node_pool WHERE node_id = $1", node)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(done.rows_affected() > 0)
    }
}
