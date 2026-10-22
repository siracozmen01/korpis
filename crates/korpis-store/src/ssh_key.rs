// SPDX-License-Identifier: AGPL-3.0-or-later

//! The SSH keys people register for SFTP (§9.9 of docs/08-identity.md).

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{SshKeyRead, SshKeyWrite, StoreError, StoreResult};
use korpis_core::account::ssh_key::{SshKey, SshPublicKey};
use std::time::SystemTime;
use uuid::Uuid;

/// A stored key's row.
struct Row {
    id: Uuid,
    account_id: Uuid,
    kind: String,
    public_key: Vec<u8>,
    label: String,
    within_id: Option<Uuid>,
    added_at: chrono::DateTime<chrono::Utc>,
    last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A stored key back as the key it was: it was checked before it was written, so it is checked
/// again on the way out, and a row that fails, or names another kind than its key is, is a
/// corrupted store, not a key.
fn key_of(r: Row) -> StoreResult<SshKey> {
    let key = SshPublicKey::from_blob(r.public_key)
        .map_err(|e| StoreError::Corrupt(format!("ssh key {}: {e}", r.id)))?;
    if key.kind().name() != r.kind {
        return Err(StoreError::Corrupt(format!("ssh key {}: stored as {}", r.id, r.kind)));
    }
    Ok(SshKey {
        id: r.id,
        account: r.account_id,
        key,
        label: r.label,
        within: r.within_id,
        added_at: to_time(r.added_at),
        last_used_at: r.last_used_at.map(to_time),
    })
}

impl SshKeyRead for PgStore {
    async fn ssh_keys(&self, account: Uuid) -> StoreResult<Vec<SshKey>> {
        let rows = sqlx::query_as!(
            Row,
            "SELECT id, account_id, kind, public_key, label, within_id, added_at, last_used_at
               FROM ssh_key WHERE account_id = $1 ORDER BY added_at, id",
            account
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(key_of).collect()
    }

    async fn ssh_key_by_fingerprint(&self, fingerprint: &str) -> StoreResult<Option<SshKey>> {
        let row = sqlx::query_as!(
            Row,
            "SELECT k.id, k.account_id, k.kind, k.public_key, k.label, k.within_id, k.added_at,
                    k.last_used_at
               FROM ssh_key k JOIN account a ON a.id = k.account_id
              WHERE k.fingerprint = $1 AND a.disabled_at IS NULL",
            fingerprint
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(key_of).transpose()
    }
}

impl SshKeyWrite for PgTx {
    async fn insert_ssh_key(&mut self, k: &SshKey) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO ssh_key (id, account_id, kind, public_key, fingerprint, label, within_id,
                                  added_at, last_used_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            k.id,
            k.account,
            k.key.kind().name(),
            k.key.blob(),
            k.key.fingerprint(),
            k.label,
            k.within,
            from_time(k.added_at),
            k.last_used_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn remove_ssh_key(&mut self, account: Uuid, id: Uuid) -> StoreResult<bool> {
        let done =
            sqlx::query!("DELETE FROM ssh_key WHERE id = $1 AND account_id = $2", id, account)
                .execute(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        Ok(done.rows_affected() == 1)
    }

    async fn ssh_key_used(&mut self, id: Uuid, at: SystemTime) -> StoreResult<()> {
        sqlx::query!("UPDATE ssh_key SET last_used_at = $2 WHERE id = $1", id, from_time(at))
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }
}
