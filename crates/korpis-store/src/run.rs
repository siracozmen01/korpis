// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs and their workloads' run state (§4 of docs/05-scheduling.md). A run is made once per
//! occurrence and per power cycle, which the schema's unique indexes hold, and changes only while
//! it is active.

use crate::errors::from_sqlx;
use crate::stored::{count, from_time, to_count, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{RunEnd, RunRead, RunWrite, StoreError, StoreResult};
use korpis_core::workload::run::{Cause, Run, RunState, RunStatus, Trigger};
use std::time::SystemTime;
use uuid::Uuid;

type Time = chrono::DateTime<chrono::Utc>;

/// One stored run.
struct RunRow {
    id: Uuid,
    workload_id: Uuid,
    intent_id: Uuid,
    trigger: String,
    scheduled_for: Time,
    started_at: Option<Time>,
    ended_at: Option<Time>,
    node_id: Option<Uuid>,
    status: String,
    exit_status: Option<i32>,
    attempt: i32,
    cycle: Option<i64>,
    occurrences: i32,
    cause: Option<String>,
    created_at: Time,
}

fn corrupt(what: &str, value: &str) -> StoreError {
    StoreError::Corrupt(format!("a run's {what} is {value:?}"))
}

fn positive(what: &str, n: i32) -> Result<u32, StoreError> {
    u32::try_from(n).map_err(|_| corrupt(what, &n.to_string()))
}

impl RunRow {
    fn into_domain(self) -> Result<Run, StoreError> {
        let cause = match self.cause {
            None => None,
            Some(c) => Some(Cause::parse(&c).ok_or_else(|| corrupt("cause", &c))?),
        };
        Ok(Run {
            id: self.id,
            workload: self.workload_id,
            intent: self.intent_id,
            trigger: Trigger::parse(&self.trigger)
                .ok_or_else(|| corrupt("trigger", &self.trigger))?,
            scheduled_for: to_time(self.scheduled_for),
            started_at: self.started_at.map(to_time),
            ended_at: self.ended_at.map(to_time),
            node: self.node_id,
            status: RunStatus::parse(&self.status)
                .ok_or_else(|| corrupt("status", &self.status))?,
            exit_status: self.exit_status,
            attempt: positive("attempt", self.attempt)?,
            cycle: self.cycle.map(|c| count("a run's cycle", c)).transpose()?,
            occurrences: positive("occurrences", self.occurrences)?,
            cause,
            created_at: to_time(self.created_at),
        })
    }
}

fn rows(rows: Vec<RunRow>) -> StoreResult<Vec<Run>> {
    rows.into_iter().map(RunRow::into_domain).collect()
}

fn state(
    workload: Uuid,
    next_due: Option<Time>,
    evaluated_from: Option<Uuid>,
    made_cycle: Option<i64>,
) -> StoreResult<RunState> {
    Ok(RunState {
        workload,
        next_due: next_due.map(to_time),
        evaluated_from,
        made_cycle: made_cycle.map(|c| count("a made cycle", c)).transpose()?,
    })
}

impl RunRead for PgStore {
    async fn run(&self, id: Uuid) -> StoreResult<Option<Run>> {
        let row = sqlx::query_as!(RunRow, "SELECT * FROM run WHERE id = $1", id)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)?;
        row.map(RunRow::into_domain).transpose()
    }

    async fn runs(&self, workload: Uuid, page: Page) -> StoreResult<Vec<Run>> {
        let found = sqlx::query_as!(
            RunRow,
            "SELECT * FROM run WHERE workload_id = $1 AND ($2::uuid IS NULL OR id < $2)
              ORDER BY id DESC LIMIT $3",
            workload,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows(found)
    }

    async fn active_runs(&self, workload: Uuid) -> StoreResult<Vec<Run>> {
        let found = sqlx::query_as!(
            RunRow,
            "SELECT * FROM run WHERE workload_id = $1 AND status IN ('pending', 'running')
              ORDER BY id",
            workload
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows(found)
    }

    async fn workloads_with_runs(&self) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            r#"SELECT w.id AS "id!" FROM workload w JOIN intent i ON i.id = w.target_id
                WHERE w.deleted_at IS NULL
                  AND i.body->>'lifecycle' IN ('LIFECYCLE_TASK', 'LIFECYCLE_SCHEDULED')
               UNION
               SELECT DISTINCT workload_id FROM run WHERE status IN ('pending', 'running')"#
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }

    async fn run_state(&self, workload: Uuid) -> StoreResult<Option<RunState>> {
        let row = sqlx::query!(
            "SELECT next_due, evaluated_from, made_cycle FROM run_state WHERE workload_id = $1",
            workload
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| state(workload, r.next_due, r.evaluated_from, r.made_cycle)).transpose()
    }
}

impl RunWrite for PgTx {
    async fn lock_run_state(&mut self, workload: Uuid) -> StoreResult<RunState> {
        sqlx::query!(
            "INSERT INTO run_state (workload_id) VALUES ($1) ON CONFLICT DO NOTHING",
            workload
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let r = sqlx::query!(
            "SELECT next_due, evaluated_from, made_cycle FROM run_state
              WHERE workload_id = $1 FOR UPDATE",
            workload
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        state(workload, r.next_due, r.evaluated_from, r.made_cycle)
    }

    async fn set_run_state(&mut self, s: &RunState) -> StoreResult<()> {
        let made = s.made_cycle.map(|c| to_count("a made cycle", c)).transpose()?;
        sqlx::query!(
            "INSERT INTO run_state (workload_id, next_due, evaluated_from, made_cycle)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (workload_id) DO UPDATE
                SET next_due = $2, evaluated_from = $3, made_cycle = $4",
            s.workload,
            s.next_due.map(from_time),
            s.evaluated_from,
            made
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn insert_run(&mut self, r: &Run) -> StoreResult<bool> {
        let small = |what: &str, n: u32| {
            i32::try_from(n).map_err(|_| StoreError::Corrupt(format!("a run's {what} is {n}")))
        };
        let inserted = sqlx::query!(
            "INSERT INTO run (id, workload_id, intent_id, trigger, scheduled_for, started_at,
                              ended_at, node_id, status, exit_status, attempt, cycle,
                              occurrences, cause, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
             ON CONFLICT DO NOTHING",
            r.id,
            r.workload,
            r.intent,
            r.trigger.as_str(),
            from_time(r.scheduled_for),
            r.started_at.map(from_time),
            r.ended_at.map(from_time),
            r.node,
            r.status.as_str(),
            r.exit_status,
            small("attempt", r.attempt)?,
            r.cycle.map(|c| to_count("a run's cycle", c)).transpose()?,
            small("occurrences", r.occurrences)?,
            r.cause.map(Cause::as_str),
            from_time(r.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(inserted.rows_affected() == 1)
    }

    async fn start_run(&mut self, id: Uuid, node: Uuid, at: SystemTime) -> StoreResult<bool> {
        let started = sqlx::query!(
            "UPDATE run SET status = 'running', node_id = $2, started_at = $3
              WHERE id = $1 AND status = 'pending'",
            id,
            node,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(started.rows_affected() == 1)
    }

    async fn end_run(&mut self, id: Uuid, end: RunEnd) -> StoreResult<bool> {
        if end.status.is_active() {
            return Err(StoreError::Corrupt(format!("a run cannot end {}", end.status.as_str())));
        }
        let ended = sqlx::query!(
            "UPDATE run SET status = $2, cause = $3, exit_status = $4, ended_at = $5
              WHERE id = $1 AND status IN ('pending', 'running')",
            id,
            end.status.as_str(),
            end.cause.as_str(),
            end.exit_status,
            from_time(end.at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(ended.rows_affected() == 1)
    }

    async fn prune_runs(
        &mut self,
        workload: Uuid,
        keep_successful: u32,
        keep_failed: u32,
    ) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "WITH ranked AS (
                SELECT id, status = 'succeeded' AS succeeded,
                       row_number() OVER (PARTITION BY status = 'succeeded'
                                          ORDER BY created_at DESC, id DESC) AS n
                  FROM run
                 WHERE workload_id = $1 AND status NOT IN ('pending', 'running'))
             DELETE FROM run r USING ranked k
              WHERE r.id = k.id
                AND ((k.succeeded AND k.n > $2) OR (NOT k.succeeded AND k.n > $3))
             RETURNING r.id",
            workload,
            i64::from(keep_successful),
            i64::from(keep_failed)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)
    }
}
