// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs of identical failed attempts to authenticate being coalesced (§10 of docs/10-api.md):
//! working state beside the effect log, as the runs of denials are.

use crate::errors::from_sqlx;
use crate::stored::{count, from_time, to_count, to_time};
use crate::{PgStore, PgTx};
use korpis_app::ports::{FailureKey, FailureRead, FailureWrite, StoreError, StoreResult};
use korpis_core::audit::denial::DenialRun;
use korpis_core::audit::failure::Attempt;
use korpis_core::authz::{Kind, ObjectRef};
use std::time::SystemTime;

/// The namespace of the per-run advisory locks, apart from every other lock's first key.
const FAILURE_LOCKS: i32 = 0x6b6f_7266;

/// A target as the table keeps it: `<kind>:<id>`, or empty.
fn target(key: &FailureKey) -> String {
    key.target.map(|t| format!("{}:{}", t.kind.as_str(), t.id)).unwrap_or_default()
}

fn key_of(source: String, action: &str, target: &str) -> StoreResult<FailureKey> {
    let corrupt = || StoreError::Corrupt(format!("failure run {action:?} on {target:?}"));
    let attempt = Attempt::from_action(action).ok_or_else(corrupt)?;
    let target = if target.is_empty() {
        None
    } else {
        let (kind, id) = target.split_once(':').ok_or_else(corrupt)?;
        let kind = Kind::parse(kind).ok_or_else(corrupt)?;
        Some(ObjectRef::new(kind, id.parse().map_err(|_| corrupt())?))
    };
    Ok(FailureKey { source, attempt, target })
}

impl FailureRead for PgStore {
    async fn failure_runs_opened_by(
        &self,
        opened_by: SystemTime,
        limit: u32,
    ) -> StoreResult<Vec<FailureKey>> {
        let rows = sqlx::query!(
            "SELECT source, action, target FROM failure_run
              WHERE opened <= $1 ORDER BY opened LIMIT $2",
            from_time(opened_by),
            i64::from(limit)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(|r| key_of(r.source, &r.action, &r.target)).collect()
    }
}

impl FailureWrite for PgTx {
    async fn lock_failure_run(&mut self, key: &FailureKey) -> StoreResult<Option<DenialRun>> {
        let (action, target) = (key.attempt.action(), target(key));
        let lock = format!("{}\n{action}\n{target}", key.source);
        sqlx::query!("SELECT pg_advisory_xact_lock($1, hashtext($2::text))", FAILURE_LOCKS, lock)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        let row = sqlx::query!(
            "SELECT opened, repeats, first_repeat, last_repeat FROM failure_run
              WHERE source = $1 AND action = $2 AND target = $3",
            key.source,
            action,
            target
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            Ok(DenialRun {
                opened: to_time(r.opened),
                repeats: count("failure repeats", r.repeats)?,
                first: r.first_repeat.map(to_time),
                last: r.last_repeat.map(to_time),
            })
        })
        .transpose()
    }

    async fn put_failure_run(&mut self, key: &FailureKey, run: &DenialRun) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO failure_run (source, action, target, opened, repeats, first_repeat,
                                      last_repeat)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (source, action, target) DO UPDATE
                SET opened = $4, repeats = $5, first_repeat = $6, last_repeat = $7",
            key.source,
            key.attempt.action(),
            target(key),
            from_time(run.opened),
            to_count("failure repeats", run.repeats)?,
            run.first.map(from_time),
            run.last.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn drop_failure_run(&mut self, key: &FailureKey) -> StoreResult<()> {
        sqlx::query!(
            "DELETE FROM failure_run WHERE source = $1 AND action = $2 AND target = $3",
            key.source,
            key.attempt.action(),
            target(key)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
