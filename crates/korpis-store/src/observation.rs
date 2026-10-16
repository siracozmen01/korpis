// SPDX-License-Identifier: AGPL-3.0-or-later

//! The latest observation per workload.

use crate::errors::from_sqlx;
use crate::stored::{ObservationDoc, count, from_json, from_time, state_str, to_json};
use crate::{PgStore, PgTx};
use korpis_app::ports::{ObservationRead, ObservationWrite, StoreResult};
use korpis_core::workload::observation::Observation;
use std::time::SystemTime;
use uuid::Uuid;

impl ObservationRead for PgStore {
    async fn observation(&self, workload: Uuid) -> StoreResult<Option<Observation>> {
        let row = sqlx::query!(
            "SELECT workload_id, node_id, observed_at, state, body
               FROM observation_latest WHERE workload_id = $1",
            workload
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(|r| {
            let doc: ObservationDoc = from_json("observation", r.body)?;
            doc.into_domain(r.workload_id, r.node_id, r.observed_at, &r.state)
        })
        .transpose()
    }

    async fn assignment_generation(&self, workload: Uuid, node: Uuid) -> StoreResult<Option<u64>> {
        let generation = sqlx::query_scalar!(
            "SELECT runs_generation FROM assignment
              WHERE workload_id = $1 AND node_id = $2 AND active",
            workload,
            node
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        generation.map(|g| count("assignment generation", g)).transpose()
    }
}

impl ObservationWrite for PgTx {
    async fn record_observation(&mut self, o: &Observation, at: SystemTime) -> StoreResult<()> {
        let body = to_json("observation", &ObservationDoc::from_domain(o))?;
        sqlx::query!(
            "INSERT INTO observation_latest (workload_id, node_id, observed_at, received_at, state,
                                             body)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (workload_id) DO UPDATE SET
                 node_id = EXCLUDED.node_id, observed_at = EXCLUDED.observed_at,
                 received_at = EXCLUDED.received_at, state = EXCLUDED.state, body = EXCLUDED.body",
            o.workload,
            o.node,
            from_time(o.observed_at),
            from_time(at),
            state_str(o.state),
            body
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
