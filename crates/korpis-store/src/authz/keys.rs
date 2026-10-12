// SPDX-License-Identifier: AGPL-3.0-or-later

//! Public halves of the control plane's signing keys. The private halves never enter the
//! database (§6 of docs/18-operations.md).

use crate::PgStore;
use crate::errors::from_sqlx;
use korpis_app::ports::{KeyRegistry, StoreResult};

impl KeyRegistry for PgStore {
    async fn register_key(&self, key_id: &str, purpose: &str, public: &[u8]) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO signing_key (key_id, purpose, public_key) VALUES ($1, $2, $3)
             ON CONFLICT (key_id) DO NOTHING",
            key_id,
            purpose,
            public
        )
        .execute(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
