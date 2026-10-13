// SPDX-License-Identifier: AGPL-3.0-or-later

//! Enrollment tokens' bindings, node identity keys, and the control plane's verify keys.

use crate::errors::from_sqlx;
use crate::stored::{from_time, principal, to_json, to_time};
use crate::tenancy::{labels, name};
use crate::{PgStore, PgTx};
use korpis_app::ports::{Enrollment, EnrollmentRead, EnrollmentWrite, StoreResult, VerifyKey};
use std::time::SystemTime;
use uuid::Uuid;

impl EnrollmentRead for PgStore {
    async fn enrollment(&self, token: Uuid) -> StoreResult<Option<Enrollment>> {
        let row = sqlx::query!(
            "SELECT token_id, grant_id, organization_id, node_name, labels, created_by,
                    created_at, expires_at, node_id
               FROM node_enrollment WHERE token_id = $1",
            token
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(Enrollment {
                token: r.token_id,
                grant: r.grant_id,
                organization: r.organization_id,
                node_name: name(r.node_name)?,
                labels: labels(r.labels)?,
                created_by: principal(&r.created_by)?,
                created_at: to_time(r.created_at),
                expires_at: to_time(r.expires_at),
                node: r.node_id,
            })
        })
        .transpose()
    }

    async fn node_key(&self, node: Uuid) -> StoreResult<Option<Vec<u8>>> {
        sqlx::query_scalar!("SELECT public_key FROM node WHERE id = $1", node)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)
    }

    async fn node_keys(&self) -> StoreResult<Vec<(Uuid, Vec<u8>)>> {
        let rows = sqlx::query!(
            "SELECT id, public_key FROM node WHERE admin <> 'decommissioned' ORDER BY id"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(|r| (r.id, r.public_key)).collect())
    }

    async fn verify_keys(&self, purpose: &str) -> StoreResult<Vec<VerifyKey>> {
        let rows = sqlx::query!(
            "SELECT key_id, public_key, not_before, not_after FROM signing_key
              WHERE purpose = $1 ORDER BY not_before, key_id",
            purpose
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows
            .into_iter()
            .map(|r| VerifyKey {
                key_id: r.key_id,
                public: r.public_key,
                not_before: to_time(r.not_before),
                not_after: r.not_after.map(to_time),
            })
            .collect())
    }
}

impl EnrollmentWrite for PgTx {
    async fn insert_enrollment(&mut self, e: &Enrollment) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO node_enrollment (token_id, grant_id, organization_id, node_name, labels,
                                          created_by, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            e.token,
            e.grant,
            e.organization,
            e.node_name.as_str(),
            to_json("enrollment labels", e.labels.as_map())?,
            e.created_by.key(),
            from_time(e.created_at),
            from_time(e.expires_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn complete_enrollment(
        &mut self,
        token: Uuid,
        node: Uuid,
        at: SystemTime,
    ) -> StoreResult<bool> {
        let done = sqlx::query!(
            "UPDATE node_enrollment SET node_id = $2, enrolled_at = $3
              WHERE token_id = $1 AND node_id IS NULL",
            token,
            node,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }
}
