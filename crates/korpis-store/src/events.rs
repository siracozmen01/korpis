// SPDX-License-Identifier: AGPL-3.0-or-later

//! The event log's upkeep (§5 of docs/15-observability.md, migration 0023): the sequencer that
//! gives events their positions once every transaction that could still commit an earlier one
//! has ended, and the pruning that keeps the log to its retention window. Events themselves are
//! written by the database, beside the effects they report.

use crate::{Error, PgStore};
use std::time::Duration;

/// How many events one pass of the sequencer positions at most.
const BATCH: i32 = 1000;

/// How often the sequencer looks without being told: a transaction that held the horizon back
/// ends without announcing anything, and what it held back is positioned on the next look.
const PERIOD: Duration = Duration::from_secs(1);

impl PgStore {
    /// Positions every event whose transactions have all ended; how many it positioned.
    ///
    /// # Errors
    /// The database's.
    pub async fn sequence_events(&self) -> Result<u64, Error> {
        let mut total = 0;
        loop {
            let n: i64 = sqlx::query_scalar("SELECT korpis_sequence_events($1)")
                .bind(BATCH)
                .fetch_one(&self.pool)
                .await?;
            total += u64::try_from(n).unwrap_or(0);
            if n < i64::from(BATCH) {
                return Ok(total);
            }
        }
    }

    /// Deletes the events up to the newest one older than `retention`; how many. What is left
    /// is contiguous, so a subscriber whose position falls before it is told of exactly the gap
    /// when it next reads (§5 of docs/15-observability.md). Effects, the audit record, are never
    /// touched.
    ///
    /// # Errors
    /// The database's.
    pub async fn prune_events(&self, retention: Duration) -> Result<u64, Error> {
        let seconds = f64::from(u32::try_from(retention.as_secs()).unwrap_or(u32::MAX));
        let done = sqlx::query(
            "DELETE FROM event
              WHERE position <= (SELECT max(position) FROM event
                                  WHERE at < now() - make_interval(secs => $1))",
        )
        .bind(seconds)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Runs the sequencer for as long as the process runs: woken by every transaction that
    /// wrote an event (through the change feed's one listening connection, when
    /// [`PgStore::listen`] runs), and once a second regardless. Several control-plane
    /// processes may each run one; they take turns on the sequencer's row.
    pub fn run_sequencer(&self) {
        tokio::spawn(sequence_forever(self.clone()));
    }
}

/// Sequences on every wake-up and every period.
async fn sequence_forever(store: PgStore) {
    loop {
        if let Err(e) = store.sequence_events().await {
            tracing::warn!(error = %e, "event sequencing failed");
        }
        // Either wakes it; a burst of wake-ups is one permit, drained by the next pass.
        let _ = tokio::time::timeout(PERIOD, store.feed.events_written.notified()).await;
    }
}
