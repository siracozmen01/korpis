// SPDX-License-Identifier: AGPL-3.0-or-later

//! API tokens. Only the SHA-256 digest of a secret is kept.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{CredentialRead, CredentialWrite, StoreResult};
use korpis_core::authz::credential::ApiToken;
use korpis_core::principal::Principal;
use std::time::SystemTime;

impl CredentialRead for PgStore {
    async fn authenticate(
        &self,
        digest: &[u8; 32],
        now: SystemTime,
    ) -> StoreResult<Option<ApiToken>> {
        let now = from_time(now);
        let row = sqlx::query!(
            "UPDATE api_token SET last_used = $2
              WHERE secret_hash = $1 AND revoked_at IS NULL
                AND (expires_at IS NULL OR expires_at > $2)
              RETURNING id, name, expires_at",
            digest.as_slice(),
            now
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| ApiToken { id: r.id, name: r.name, expires_at: r.expires_at.map(to_time) }))
    }
}

impl CredentialWrite for PgTx {
    async fn insert_token(
        &mut self,
        token: &ApiToken,
        digest: &[u8; 32],
        created_by: &Principal,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO api_token (id, name, secret_hash, created_by, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
            token.id,
            token.name,
            digest.as_slice(),
            created_by.key(),
            token.expires_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
