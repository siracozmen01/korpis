// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider circuit breakers (§5 of docs/16-extensions.md, migration 0028).

use crate::PgTx;
use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use chrono::{DateTime, Utc};
use korpis_app::ports::{BreakerWrite, ProviderKey, StoreError, StoreResult};
use korpis_core::extension::{Breaker, ProviderChange, ProviderCondition, Sample, WINDOW_CALLS};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

struct BreakerRow {
    state: String,
    until: Option<DateTime<Utc>>,
    backoff_ms: Option<i64>,
    probe: Option<Uuid>,
}

impl BreakerRow {
    fn into_domain(self) -> StoreResult<Breaker> {
        let corrupt = || StoreError::Corrupt(format!("provider breaker {:?}", self.state));
        let backoff =
            self.backoff_ms.map(|ms| Duration::from_millis(u64::try_from(ms).unwrap_or_default()));
        Ok(match (self.state.as_str(), self.until, backoff, self.probe) {
            ("closed", ..) => Breaker::Closed,
            ("open", Some(until), Some(backoff), None) => {
                Breaker::Open { until: to_time(until), backoff }
            }
            ("half_open", Some(until), Some(backoff), Some(probe)) => {
                Breaker::HalfOpen { probe, until: to_time(until), backoff }
            }
            _ => return Err(corrupt()),
        })
    }
}

fn columns(b: &Breaker) -> (&'static str, Option<DateTime<Utc>>, Option<i64>, Option<Uuid>) {
    let ms = |d: &Duration| i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
    match b {
        Breaker::Closed => ("closed", None, None, None),
        Breaker::Open { until, backoff } => {
            ("open", Some(from_time(*until)), Some(ms(backoff)), None)
        }
        Breaker::HalfOpen { probe, until, backoff } => {
            ("half_open", Some(from_time(*until)), Some(ms(backoff)), Some(*probe))
        }
    }
}

fn detail(c: &ProviderCondition) -> serde_json::Value {
    match c.change {
        ProviderChange::Degraded { trip, measured: m, queued } => serde_json::json!({
            "type": c.kind.as_str(),
            "name": c.name,
            "reason": trip.as_str(),
            "calls": m.calls,
            "failed": m.failed,
            "median_latency_ms": u64::try_from(m.median_elapsed.as_millis()).unwrap_or(u64::MAX),
            "median_deadline_permille": m.median_share,
            "queued": queued,
        }),
        ProviderChange::Recovered => serde_json::json!({ "type": c.kind.as_str(), "name": c.name }),
    }
}

impl BreakerWrite for PgTx {
    async fn lock_breaker(&mut self, p: ProviderKey<'_>, at: SystemTime) -> StoreResult<Breaker> {
        sqlx::query!(
            "INSERT INTO provider_breaker (organization_id, type, name, state, changed_at)
             VALUES ($1, $2, $3, 'closed', $4) ON CONFLICT DO NOTHING",
            p.organization,
            p.kind.as_str(),
            p.name,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let row = sqlx::query_as!(
            BreakerRow,
            "SELECT state, until, backoff_ms, probe FROM provider_breaker
              WHERE organization_id = $1 AND type = $2 AND name = $3 FOR UPDATE",
            p.organization,
            p.kind.as_str(),
            p.name
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.into_domain()
    }

    async fn read_breaker(&mut self, p: ProviderKey<'_>) -> StoreResult<Breaker> {
        let row = sqlx::query_as!(
            BreakerRow,
            "SELECT state, until, backoff_ms, probe FROM provider_breaker
              WHERE organization_id = $1 AND type = $2 AND name = $3",
            p.organization,
            p.kind.as_str(),
            p.name
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map_or(Ok(Breaker::Closed), BreakerRow::into_domain)
    }

    async fn set_breaker(
        &mut self,
        p: ProviderKey<'_>,
        b: &Breaker,
        at: SystemTime,
    ) -> StoreResult<()> {
        let (state, until, backoff_ms, probe) = columns(b);
        sqlx::query!(
            "INSERT INTO provider_breaker (organization_id, type, name, state, until, backoff_ms,
                                           probe, changed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (organization_id, type, name) DO UPDATE
                SET state = $4, until = $5, backoff_ms = $6, probe = $7, changed_at = $8",
            p.organization,
            p.kind.as_str(),
            p.name,
            state,
            until,
            backoff_ms,
            probe,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn recent_samples(
        &mut self,
        p: ProviderKey<'_>,
        since: SystemTime,
    ) -> StoreResult<Vec<Sample>> {
        // A call that failed because nothing provides it any more says nothing of the provider.
        let rows = sqlx::query!(
            r#"SELECT coalesce((outcome -> 'failed' ->> 'retryable')::boolean, false) AS "failed!",
                      created_at, deadline, finished_at AS "finished_at!"
                 FROM provider_call
                WHERE organization_id = $1 AND type = $2 AND name = $3
                  AND finished_at IS NOT NULL AND finished_at >= $4
                  AND coalesce(outcome -> 'failed' ->> 'reason', '') <> 'provider.unknown'
                ORDER BY finished_at DESC
                LIMIT $5"#,
            p.organization,
            p.kind.as_str(),
            p.name,
            from_time(since),
            i64::try_from(WINDOW_CALLS).unwrap_or(i64::MAX)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        let span = |from: DateTime<Utc>, to: DateTime<Utc>| {
            to_time(to).duration_since(to_time(from)).unwrap_or_default()
        };
        Ok(rows
            .into_iter()
            .map(|r| Sample {
                failed: r.failed,
                elapsed: span(r.created_at, r.finished_at),
                allowed: span(r.created_at, r.deadline),
            })
            .collect())
    }

    async fn queued_calls(&mut self, p: ProviderKey<'_>) -> StoreResult<u64> {
        let n = sqlx::query_scalar!(
            r#"SELECT count(*) AS "n!" FROM provider_call
                WHERE organization_id = $1 AND type = $2 AND name = $3
                  AND registration_id IS NULL AND finished_at IS NULL"#,
            p.organization,
            p.kind.as_str(),
            p.name
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        crate::stored::count("queued calls", n)
    }

    async fn append_provider_condition(&mut self, c: &ProviderCondition) -> StoreResult<()> {
        sqlx::query!(
            "SELECT korpis_append_event($1, $2, $3, $4, $5, NULL, NULL, $6)",
            c.key(),
            from_time(c.at),
            c.event_kind(),
            c.severity().as_str(),
            c.organization,
            detail(c)
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
