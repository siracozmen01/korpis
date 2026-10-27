// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume made from its replica (§2.2 of docs/06-storage.md): its record moves to the
//! replica's node under the identifier it is made under there, which keeps the replica's
//! snapshot until it reports the volume made. The copy it leaves is a volume of its own on the
//! node it left, retained and held by no workload, with the volume's snapshots, which are there;
//! and every other replica is sent whole next, since what it holds is no base the new node
//! keeps.

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use korpis_app::ports::StoreResult;
use korpis_core::workload::replica::Divergent;
use korpis_core::workload::transfers::Promoting;
use korpis_core::workload::volume::Volume;
use std::time::SystemTime;
use uuid::Uuid;

/// One copy left on a node.
struct DivergentRow {
    id: Uuid,
    divergent_of: Uuid,
    node_id: Uuid,
    since: chrono::DateTime<chrono::Utc>,
    held_until: chrono::DateTime<chrono::Utc>,
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Fills in the live copies each of `volumes` left on nodes it failed over from, and for each
/// that is such a copy, the volume it diverged from.
pub(super) async fn with_divergent(pool: &sqlx::PgPool, volumes: &mut [Volume]) -> StoreResult<()> {
    let ids: Vec<Uuid> = volumes.iter().map(|v| v.id).collect();
    let rows = sqlx::query_file_as!(DivergentRow, "queries/divergent_of_volumes.sql", &ids)
        .fetch_all(pool)
        .await
        .map_err(from_sqlx)?;
    for r in rows {
        if let Some(v) = volumes.iter_mut().find(|v| v.id == r.id) {
            v.divergent_of = Some(r.divergent_of);
        }
        let live = r.deleted_at.is_none();
        if let Some(v) = volumes.iter_mut().find(|v| v.id == r.divergent_of && live) {
            v.divergent.push(Divergent {
                volume: r.id,
                node: r.node_id,
                since: to_time(r.since),
                held_until: to_time(r.held_until),
            });
        }
    }
    Ok(())
}

/// Every live volume being made from its replica on its node, not yet reported made there.
pub(super) async fn promoting(pool: &sqlx::PgPool) -> StoreResult<Vec<Promoting>> {
    let rows = sqlx::query!(
        r#"SELECT id, node_id, storage_class, promoting AS "promoting!"
             FROM volume
            WHERE promoting IS NOT NULL AND deleted_at IS NULL AND store IS NULL
            ORDER BY id"#
    )
    .fetch_all(pool)
    .await
    .map_err(from_sqlx)?;
    let made = rows.into_iter().map(|r| Promoting {
        volume: r.id,
        node: r.node_id,
        storage_class: r.storage_class,
        snapshot: r.promoting,
    });
    Ok(made.collect())
}

/// Moves `volume` from `from` to `to`, where it is made as `into` from the replica's snapshot
/// `snapshot`, which it received at `held_until`, at `now`; whether it was on `from`.
pub(super) async fn promote(
    tx: &mut PgTx,
    (volume, from, to): (Uuid, Uuid, Uuid),
    (into, snapshot, held_until): (&str, Uuid, SystemTime),
    now: SystemTime,
) -> StoreResult<bool> {
    let left = sqlx::query_scalar!(
        "SELECT helper_id FROM volume
          WHERE id = $1 AND node_id = $2 AND deleted_at IS NULL AND store IS NULL
          FOR UPDATE",
        volume,
        from
    )
    .fetch_optional(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    let Some(helper_id) = left else { return Ok(false) };
    sqlx::query!(
        "UPDATE volume SET node_id = $2, helper_id = $3, promoting = $4 WHERE id = $1",
        volume,
        to,
        into,
        snapshot
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    let copy = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO volume (id, project_id, created_by, attached_to, name, kind, node_id,
                             helper_id, storage_class, size_bytes, quota_enforced, created_at,
                             detached_at, divergent_of, divergent_since, held_until)
         SELECT $2, project_id, created_by, NULL, name, kind, $3, $4, storage_class, size_bytes,
                quota_enforced, created_at, $5, id, $5, $6
           FROM volume WHERE id = $1",
        volume,
        copy,
        from,
        helper_id,
        from_time(now),
        from_time(held_until)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    left_behind(tx, (volume, copy, to), now).await?;
    Ok(true)
}

/// What stays with the copy `copy` that `volume` left, now on `to`, at `now`: its snapshots,
/// which are where the copy is; the replica on `to` is no longer one, and every other is stale.
async fn left_behind(
    tx: &mut PgTx,
    (volume, copy, to): (Uuid, Uuid, Uuid),
    now: SystemTime,
) -> StoreResult<()> {
    sqlx::query!(
        "UPDATE snapshot SET volume_id = $2 WHERE volume_id = $1 AND deleted_at IS NULL",
        volume,
        copy
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    sqlx::query!("DELETE FROM volume_replica WHERE volume_id = $1 AND node_id = $2", volume, to)
        .execute(&mut *tx.tx)
        .await
        .map_err(from_sqlx)?;
    sqlx::query!(
        "UPDATE volume_replica SET stale_at = coalesce(stale_at, $2), sending = NULL,
                                   sending_since = NULL
          WHERE volume_id = $1",
        volume,
        from_time(now)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}
