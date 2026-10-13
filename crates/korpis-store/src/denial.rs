// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs of identical denials being coalesced (§10 of docs/10-api.md): working state beside the
//! effect log, never part of it.

use crate::errors::from_sqlx;
use crate::stored::{count, from_time, principal, to_count, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{DenialKey, DenialRead, DenialWrite, StoreError, StoreResult};
use korpis_core::audit::denial::DenialRun;
use korpis_core::authz::{Kind, ObjectRef};
use std::time::SystemTime;

/// The namespace of the per-run advisory locks, apart from every other lock's first key.
const DENIAL_LOCKS: i32 = 0x6b6f_7264;

impl DenialRead for PgStore {
    async fn denial_runs_opened_by(
        &self,
        opened_by: SystemTime,
        limit: u32,
    ) -> StoreResult<Vec<DenialKey>> {
        let rows = sqlx::query!(
            "SELECT actor, action, object_kind, object_id FROM denial_run
              WHERE opened <= $1 ORDER BY opened LIMIT $2",
            from_time(opened_by),
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| {
                let kind = Kind::parse(&r.object_kind).ok_or_else(|| {
                    StoreError::Corrupt(format!("denial object kind {:?}", r.object_kind))
                })?;
                Ok(DenialKey {
                    actor: principal(&r.actor)?,
                    action: r.action,
                    object: ObjectRef::new(kind, r.object_id),
                })
            })
            .collect()
    }
}

impl DenialWrite for PgTx {
    async fn lock_denial_run(&mut self, key: &DenialKey) -> StoreResult<Option<DenialRun>> {
        let (actor, kind) = (key.actor.key(), key.object.kind.as_str());
        let lock = format!("{actor}\n{}\n{kind}:{}", key.action, key.object.id);
        sqlx::query!("SELECT pg_advisory_xact_lock($1, hashtext($2::text))", DENIAL_LOCKS, lock)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let row = sqlx::query!(
            "SELECT opened, repeats, first_repeat, last_repeat FROM denial_run
              WHERE actor = $1 AND action = $2 AND object_kind = $3 AND object_id = $4",
            actor,
            key.action,
            kind,
            key.object.id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(DenialRun {
                opened: to_time(r.opened),
                repeats: count("denial repeats", r.repeats)?,
                first: r.first_repeat.map(to_time),
                last: r.last_repeat.map(to_time),
            })
        })
        .transpose()
    }

    async fn put_denial_run(&mut self, key: &DenialKey, run: &DenialRun) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO denial_run (actor, action, object_kind, object_id, opened, repeats,
                                     first_repeat, last_repeat)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (actor, action, object_kind, object_id) DO UPDATE
                SET opened = $5, repeats = $6, first_repeat = $7, last_repeat = $8",
            key.actor.key(),
            key.action,
            key.object.kind.as_str(),
            key.object.id,
            from_time(run.opened),
            to_count("denial repeats", run.repeats)?,
            run.first.map(from_time),
            run.last.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn drop_denial_run(&mut self, key: &DenialKey) -> StoreResult<()> {
        sqlx::query!(
            "DELETE FROM denial_run
              WHERE actor = $1 AND action = $2 AND object_kind = $3 AND object_id = $4",
            key.actor.key(),
            key.action,
            key.object.kind.as_str(),
            key.object.id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
