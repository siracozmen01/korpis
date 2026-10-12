// SPDX-License-Identifier: AGPL-3.0-or-later

//! Idempotency keys (§6 of docs/10-api.md).

use crate::errors::from_sqlx;
use crate::{PgStore, PgTx};
use korpis_app::ports::{IdempotencyRead, IdempotencyWrite, Recorded, StoreResult};

impl IdempotencyRead for PgStore {
    async fn recorded(&self, subject_key: &str, key: &str) -> StoreResult<Option<Recorded>> {
        let row = sqlx::query!(
            "SELECT method, response FROM idempotency WHERE subject_key = $1 AND key = $2",
            subject_key,
            key
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| Recorded { operation: r.method, response: r.response }))
    }
}

impl IdempotencyWrite for PgTx {
    async fn record(
        &mut self,
        subject_key: &str,
        key: &str,
        recorded: &Recorded,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO idempotency (subject_key, key, method, response) VALUES ($1, $2, $3, $4)",
            subject_key,
            key,
            recorded.operation,
            recorded.response
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
