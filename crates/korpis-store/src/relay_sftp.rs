// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where each relay serving SFTP is reached, and the host key it serves, as the relay last
//! reported them (§3.1 of docs/02-architecture.md); the relay store's impls call these.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{RelaySftp, StoreResult};
use uuid::Uuid;

pub(crate) async fn read(store: &PgStore) -> StoreResult<Vec<RelaySftp>> {
    let rows = sqlx::query!(
        "SELECT relay_id, address, host_key, reported_at FROM relay_sftp ORDER BY relay_id"
    )
    .fetch_all(&store.pool)
    .await
    .map_err(from_sqlx)?;
    Ok(rows
        .into_iter()
        .map(|r| RelaySftp {
            relay: r.relay_id,
            address: r.address,
            host_key: r.host_key,
            reported_at: to_time(r.reported_at),
        })
        .collect())
}

pub(crate) async fn set(tx: &mut PgTx, s: &RelaySftp) -> StoreResult<()> {
    sqlx::query!(
        "INSERT INTO relay_sftp (relay_id, address, host_key, reported_at) VALUES ($1, $2, $3, $4)
         ON CONFLICT (relay_id) DO UPDATE
            SET address = excluded.address, host_key = excluded.host_key,
                reported_at = excluded.reported_at",
        s.relay,
        s.address,
        s.host_key,
        from_time(s.reported_at)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

pub(crate) async fn clear(tx: &mut PgTx, relay: Uuid) -> StoreResult<()> {
    sqlx::query!("DELETE FROM relay_sftp WHERE relay_id = $1", relay)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    Ok(())
}
