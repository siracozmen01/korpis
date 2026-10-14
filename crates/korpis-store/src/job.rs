// SPDX-License-Identifier: AGPL-3.0-or-later

//! The job queue (§6 of docs/03-state.md, migration 0043). A claim is one statement: the oldest
//! due job of the kinds asked for, locked with SKIP LOCKED so two replicas never take the same,
//! marked running under the claimer's name and lease, its attempt counted. A job whose lease ran
//! out is due again, so one whose replica died is taken by another.

use crate::errors::from_sqlx;
use crate::stored::from_time;
use crate::workload::rows::PlanRow;
use crate::{PgStore, PgTx};
use korpis_app::ports::{Job, JobEnd, JobRead, JobWrite, NewJob, StoreResult};
use korpis_core::workload::Plan;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn count(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

impl JobWrite for PgTx {
    async fn enqueue_job(&mut self, job: &NewJob<'_>) -> StoreResult<bool> {
        let max = i32::try_from(job.max_attempts).unwrap_or(i32::MAX);
        let queued = sqlx::query!(
            "INSERT INTO job (id, key, kind, payload, run_after, max_attempts, status, created)
             VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7)
             ON CONFLICT (key) DO NOTHING",
            Uuid::now_v7(),
            job.key,
            job.kind,
            job.payload,
            from_time(job.due),
            max,
            from_time(job.now)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(queued.rows_affected() == 1)
    }

    async fn end_job(
        &mut self,
        (id, worker): (Uuid, &str),
        end: &JobEnd,
        now: SystemTime,
    ) -> StoreResult<bool> {
        let (status, run_after, error) = match end {
            JobEnd::Done => ("succeeded", None, None),
            JobEnd::Retry { at, error } => ("pending", Some(from_time(*at)), Some(error.as_str())),
            JobEnd::Failed(error) => ("failed", None, Some(error.as_str())),
            JobEnd::Defer { at } => ("deferred", Some(from_time(*at)), None),
        };
        let ended = sqlx::query_file!(
            "queries/job_end.sql",
            id,
            worker,
            status,
            run_after,
            error.map(|e| serde_json::Value::String(e.to_owned())),
            from_time(now)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(ended.rows_affected() == 1)
    }
}

impl JobRead for PgStore {
    async fn control_plane_plans(&self) -> StoreResult<Vec<Plan>> {
        let rows = sqlx::query_file_as!(PlanRow, "queries/plans_awaiting_control_plane.sql")
            .fetch_all(&self.pool)
            .await
            .map_err(from_sqlx)?;
        rows.into_iter().map(PlanRow::into_domain).collect()
    }

    async fn claim_job(
        &self,
        worker: &str,
        kinds: &[&str],
        (now, lease): (SystemTime, Duration),
    ) -> StoreResult<Option<Job>> {
        let kinds: Vec<String> = kinds.iter().map(|k| (*k).to_owned()).collect();
        let until = now + lease;
        let row = sqlx::query_file!(
            "queries/job_claim.sql",
            &kinds,
            from_time(now),
            worker,
            from_time(until)
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        Ok(row.map(|r| Job {
            id: r.id,
            kind: r.kind,
            payload: r.payload,
            attempt: count(r.attempts),
            max_attempts: count(r.max_attempts),
        }))
    }
}
