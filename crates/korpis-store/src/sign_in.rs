// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pending sign-ins (migrations 0102 and 0113).

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use chrono::{DateTime, Utc};
use korpis_app::ports::{SignInWrite, StoreResult};
use korpis_core::authz::binding::ProviderRef;
use korpis_core::authz::sign_in::PendingSignIn;
use std::time::SystemTime;
use uuid::Uuid;

struct PendingRow {
    organization_id: Uuid,
    provider: String,
    nonce: String,
    code_challenge: String,
    redirect_uri: String,
    portal_id: Option<Uuid>,
    begun_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl PendingRow {
    fn into_domain(self) -> PendingSignIn {
        PendingSignIn {
            provider: ProviderRef { organization: self.organization_id, name: self.provider },
            nonce: self.nonce,
            code_challenge: self.code_challenge,
            redirect_uri: self.redirect_uri,
            portal: self.portal_id,
            begun_at: to_time(self.begun_at),
            expires_at: to_time(self.expires_at),
        }
    }
}

impl SignInWrite for PgTx {
    async fn insert_pending_sign_in(
        &mut self,
        state_digest: &[u8; 32],
        p: &PendingSignIn,
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO pending_sign_in
                (state_digest, organization_id, provider, nonce, code_challenge, redirect_uri,
                 portal_id, begun_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            &state_digest[..],
            p.provider.organization,
            p.provider.name,
            p.nonce,
            p.code_challenge,
            p.redirect_uri,
            p.portal,
            from_time(p.begun_at),
            from_time(p.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn spend_pending_sign_in(
        &mut self,
        state_digest: &[u8; 32],
        now: SystemTime,
    ) -> StoreResult<Option<PendingSignIn>> {
        let row = sqlx::query_as!(
            PendingRow,
            "DELETE FROM pending_sign_in WHERE state_digest = $1 AND expires_at > $2
             RETURNING organization_id, provider, nonce, code_challenge, redirect_uri, portal_id,
                       begun_at, expires_at",
            &state_digest[..],
            from_time(now)
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(PendingRow::into_domain))
    }

    async fn forget_pending_sign_ins(&mut self, before: SystemTime) -> StoreResult<u64> {
        let done =
            sqlx::query!("DELETE FROM pending_sign_in WHERE expires_at < $1", from_time(before))
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        Ok(done.rows_affected())
    }
}
