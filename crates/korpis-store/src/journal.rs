// SPDX-License-Identifier: AGPL-3.0-or-later

//! The per-node journal cursor.

use crate::errors::from_sqlx;
use crate::stored::{count, to_count};
use crate::{PgStore, PgTx};
use korpis_app::ports::{JournalRead, JournalWrite, StoreResult};
use uuid::Uuid;

impl JournalRead for PgStore {
    async fn journal_ingested(&self, node: Uuid) -> StoreResult<u64> {
        let through = sqlx::query_scalar!(
            "SELECT ingested_through FROM node_journal_cursor WHERE node_id = $1",
            node
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        through.map_or(Ok(0), |t| count("journal cursor", t))
    }
}

impl JournalWrite for PgTx {
    async fn lock_journal(&mut self, node: Uuid) -> StoreResult<u64> {
        sqlx::query!(
            "INSERT INTO node_journal_cursor (node_id) VALUES ($1) ON CONFLICT DO NOTHING",
            node
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let through = sqlx::query_scalar!(
            "SELECT ingested_through FROM node_journal_cursor WHERE node_id = $1 FOR UPDATE",
            node
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        count("journal cursor", through)
    }

    async fn advance_journal(&mut self, node: Uuid, through: u64) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE node_journal_cursor SET ingested_through = $2 WHERE node_id = $1",
            node,
            to_count("journal cursor", through)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
