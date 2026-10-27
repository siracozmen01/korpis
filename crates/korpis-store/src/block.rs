// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's devices (migration 0143, §2.1 of docs/06-storage.md). A device is due a call
//! when it is not made yet; when its workload holds its lease on the node it is placed on for
//! another node or epoch than the device was attached for, or holds none for the one it was
//! attached for; or when it is gone; and only when its backoff has passed and no control plane
//! holds it (`queries/block_device_due.sql`). One is taken at a time, skipping those another
//! transaction has locked, and held for a while, so that one control plane asks.

use crate::errors::from_sqlx;
use crate::stored::{count, from_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{
    BlockRead, BlockWrite, HeldDevice, SealedAttachment, SealedKey, StoreError, StoreResult,
};
use korpis_core::extension::block::BlockProtocol;
use korpis_core::workload::block::{DeviceStatus, DueDevice, WantedDevice};
use std::time::SystemTime;
use uuid::Uuid;

fn protocol(s: &str) -> StoreResult<BlockProtocol> {
    BlockProtocol::parse(s).ok_or_else(|| StoreError::Corrupt(format!("block protocol {s:?}")))
}

fn size(bytes: u64) -> StoreResult<i64> {
    i64::try_from(bytes).map_err(|_| StoreError::Corrupt(format!("device size {bytes}")))
}

impl BlockRead for PgStore {
    async fn placed_on_block(&self, page: Page) -> StoreResult<Vec<(Uuid, Uuid)>> {
        let rows = sqlx::query!(
            "SELECT p.workload_id, p.node_id FROM placement p JOIN node n ON n.id = p.node_id
              WHERE p.authoritative
                AND EXISTS (SELECT 1 FROM jsonb_array_elements(n.storage_classes) c
                             WHERE c ? 'block')
                AND ($1::uuid IS NULL OR p.workload_id > $1)
              ORDER BY p.workload_id LIMIT $2",
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows.into_iter().map(|r| (r.workload_id, r.node_id)).collect())
    }

    async fn held_devices(
        &self,
        workload: Uuid,
        (node, epoch): (Uuid, u64),
    ) -> StoreResult<Vec<HeldDevice>> {
        let epoch = i64::try_from(epoch).map_err(|_| StoreError::Corrupt("epoch".into()))?;
        let rows = sqlx::query!(
            r#"SELECT id, helper_id, protocol AS "protocol!", target AS "target!", credential,
                      sealed_by
                 FROM block_device
                WHERE workload_id = $1 AND attached_node_id = $2 AND attached_epoch = $3
                  AND deleted_at IS NULL
                ORDER BY helper_id"#,
            workload,
            node,
            epoch
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let credential = r.credential.zip(r.sealed_by);
                Ok(HeldDevice {
                    id: r.id,
                    helper_id: r.helper_id,
                    attachment: SealedAttachment {
                        protocol: protocol(&r.protocol)?,
                        target: r.target,
                        credential: credential
                            .map(|(sealed, sealed_by)| SealedKey { sealed_by, sealed }),
                    },
                })
            })
            .collect()
    }

    async fn device_status(
        &self,
        workload: Uuid,
        now: SystemTime,
    ) -> StoreResult<Vec<DeviceStatus>> {
        let rows = sqlx::query!(
            r#"SELECT b.name, b.storage_class, b.device IS NOT NULL AS "made!", b.reason,
                      EXISTS (SELECT 1 FROM lease l
                               WHERE l.workload_id = b.workload_id
                                 AND l.node_id = b.attached_node_id
                                 AND l.epoch = b.attached_epoch
                                 AND l.released_at IS NULL AND l.grantor_expiry > $2)
                          AS "attached!"
                 FROM block_device b
                WHERE b.workload_id = $1 AND b.deleted_at IS NULL
                ORDER BY b.name"#,
            workload,
            from_time(now)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(rows
            .into_iter()
            .map(|r| DeviceStatus {
                name: r.name,
                storage_class: r.storage_class,
                made: r.made,
                attached: r.attached,
                reason: r.reason,
            })
            .collect())
    }
}

impl BlockWrite for PgTx {
    async fn want_device(&mut self, d: &WantedDevice) -> StoreResult<bool> {
        let inserted = sqlx::query_scalar!(
            r#"INSERT INTO block_device (id, organization_id, workload_id, store, helper_id,
                                         name, storage_class, size_bytes, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
               ON CONFLICT (store, helper_id) WHERE deleted_at IS NULL
               DO UPDATE SET workload_id = EXCLUDED.workload_id, name = EXCLUDED.name
                  WHERE block_device.workload_id <> EXCLUDED.workload_id
               RETURNING (xmax = 0) AS "inserted!""#,
            d.id,
            d.organization,
            d.workload,
            d.store,
            d.helper_id,
            d.name,
            d.storage_class,
            size(d.size_bytes)?
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(inserted.unwrap_or(false))
    }

    async fn take_due_device(
        &mut self,
        now: SystemTime,
        until: SystemTime,
    ) -> StoreResult<Option<DueDevice>> {
        let row =
            sqlx::query_file!("queries/block_device_due.sql", from_time(now), from_time(until))
                .fetch_optional(&mut *self.tx)
                .await
                .map_err(from_sqlx)?;
        let Some(r) = row else { return Ok(None) };
        let wanted = match (r.lease_node, r.lease_epoch, r.protocol) {
            (Some(n), Some(e), Some(p)) => Some((n, count("lease epoch", e)?, protocol(&p)?)),
            _ => None,
        };
        let attached = match (r.attached_node_id, r.attached_epoch) {
            (Some(n), Some(e)) => Some((n, count("attached epoch", e)?)),
            _ => None,
        };
        Ok(Some(DueDevice {
            id: r.id,
            organization: r.organization_id,
            storage_class: r.storage_class,
            size_bytes: count("device size", r.size_bytes)?,
            device: r.device,
            wanted,
            attached,
            gone: r.gone,
            attempts: u32::try_from(r.attempts).unwrap_or(0),
        }))
    }

    async fn device_created(&mut self, id: Uuid, device: &str) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE block_device SET device = $2, reason = NULL, attempts = 0, due = NULL,
                                     held_until = NULL
              WHERE id = $1",
            id,
            device
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn device_attached(
        &mut self,
        id: Uuid,
        (node, epoch): (Uuid, u64),
        a: &SealedAttachment,
    ) -> StoreResult<()> {
        let epoch = i64::try_from(epoch).map_err(|_| StoreError::Corrupt("epoch".into()))?;
        let (credential, sealed_by) = match &a.credential {
            Some(k) => (Some(k.sealed.clone()), Some(k.sealed_by.clone())),
            None => (None, None),
        };
        sqlx::query!(
            "UPDATE block_device
                SET attached_node_id = $2, attached_epoch = $3, protocol = $4, target = $5,
                    credential = $6, sealed_by = $7, reason = NULL, attempts = 0, due = NULL,
                    held_until = NULL
              WHERE id = $1",
            id,
            node,
            epoch,
            a.protocol.as_str(),
            a.target,
            credential,
            sealed_by
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn device_detached(&mut self, id: Uuid) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE block_device
                SET attached_node_id = NULL, attached_epoch = NULL, protocol = NULL,
                    target = NULL, credential = NULL, sealed_by = NULL, reason = NULL,
                    attempts = 0, due = NULL, held_until = NULL
              WHERE id = $1",
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn device_deleted(&mut self, id: Uuid, now: SystemTime) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE block_device
                SET deleted_at = $2, reason = NULL, attempts = 0, due = NULL, held_until = NULL
              WHERE id = $1",
            id,
            from_time(now)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn device_failed(
        &mut self,
        id: Uuid,
        (reason, attempts): (&str, u32),
        due: SystemTime,
    ) -> StoreResult<()> {
        sqlx::query!(
            "UPDATE block_device
                SET reason = $2, attempts = $3, due = $4, held_until = NULL
              WHERE id = $1",
            id,
            reason,
            i32::try_from(attempts).unwrap_or(i32::MAX),
            from_time(due)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
