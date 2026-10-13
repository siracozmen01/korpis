// SPDX-License-Identifier: AGPL-3.0-or-later

//! API tokens and workload credentials. Only the SHA-256 digest of a secret is kept.

use crate::errors::from_sqlx;
use crate::stored::{count, from_time, principal, to_count, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{CredentialRead, CredentialWrite, StoreResult};
use korpis_core::authz::credential::{ApiToken, WorkloadCredential};
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

    async fn authenticate_workload(
        &self,
        digest: &[u8; 32],
        now: SystemTime,
    ) -> StoreResult<Option<WorkloadCredential>> {
        let row = sqlx::query!(
            "SELECT c.workload_id, c.subject, c.node_id, c.epoch, c.issued_at, c.expires_at
               FROM workload_credential c
               JOIN placement p ON p.workload_id = c.workload_id AND p.node_id = c.node_id
               JOIN workload w ON w.id = c.workload_id
              WHERE c.secret_hash = $1 AND c.expires_at > $2
                AND p.authoritative AND w.deleted_at IS NULL",
            digest.as_slice(),
            from_time(now)
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(WorkloadCredential {
                workload: r.workload_id,
                subject: principal(&r.subject)?,
                node: r.node_id,
                epoch: count("credential epoch", r.epoch)?,
                issued_at: to_time(r.issued_at),
                expires_at: to_time(r.expires_at),
            })
        })
        .transpose()
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

    async fn insert_workload_credential(
        &mut self,
        c: &WorkloadCredential,
        digest: &[u8; 32],
    ) -> StoreResult<()> {
        sqlx::query!(
            "DELETE FROM workload_credential WHERE expires_at <= $1",
            from_time(c.issued_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        sqlx::query!(
            "INSERT INTO workload_credential
                    (secret_hash, workload_id, subject, node_id, epoch, issued_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            digest.as_slice(),
            c.workload,
            c.subject.key(),
            c.node,
            to_count("credential epoch", c.epoch)?,
            from_time(c.issued_at),
            from_time(c.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
