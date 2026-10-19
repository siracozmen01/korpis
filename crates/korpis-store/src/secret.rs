// SPDX-License-Identifier: AGPL-3.0-or-later

//! Secrets every control plane of an installation shares (migration 0093).

use crate::PgStore;
use crate::errors::from_sqlx;
use korpis_app::ports::{StoreError, StoreResult};

impl PgStore {
    /// The installation's secret `name`: `fresh` if no control plane has made it yet, which it
    /// then is, and what the first made otherwise.
    pub async fn server_secret(&self, name: &str, fresh: &[u8; 32]) -> StoreResult<[u8; 32]> {
        sqlx::query!(
            "INSERT INTO server_secret (name, secret) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
            name,
            fresh.as_slice()
        )
        .execute(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let kept = sqlx::query_scalar!("SELECT secret FROM server_secret WHERE name = $1", name)
            .fetch_one(&self.pool)
            .await
            .map_err(from_sqlx)?;
        <[u8; 32]>::try_from(kept.as_slice())
            .map_err(|_| StoreError::Corrupt(format!("server secret {name} is not 32 bytes")))
    }
}
