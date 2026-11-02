// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's metering samples ("Measuring it, as designed"): each is measured from the last sample
//! of its workload on that node, and what it measured is appended as a record, superseding every
//! gap it covers, and an adjustment where the day it begins in is closed. A sample is believed only
//! from a node that held the workload, and not past the workload's move off it; it is not an
//! effect: metering is not the audit log, and the record is its own. Only a sample that is not
//! believed leaves an effect, as a rejection.

use super::rejected;
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, MeteringWrite as _, RecipeSource, Store};
use korpis_core::audit::Effect;
use korpis_core::metering::{Reading, Record, RecordKind, Sample, day_of, implausible, measure};
use korpis_core::workload::migration::CLOCK_MARGIN;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Whether `sample`, from `node`, which holds the workload under `current` if under any, runs
    /// past the epoch it was taken under: a migration moved the workload off the node, and the
    /// sample, under an epoch the node no longer holds, ends after the move by more than the two
    /// clocks may disagree. The destination's first sample counts from nothing, since none before
    /// it is from that node, so the meter is cut at the epoch and no interval is counted twice.
    async fn past_the_move(
        &self,
        node: Uuid,
        sample: &Sample,
        current: Option<u64>,
    ) -> Result<bool, AppError> {
        if current == Some(sample.lease_epoch) {
            return Ok(false);
        }
        // The move that ended the sample's epoch is the first off this node after the sample
        // began, as either clock may tell it: a workload that came back and left again was moved
        // again, and a late sample of the first stay is cut at the first move.
        let migrations = self.store.migrations_of(sample.workload).await?;
        let ended = migrations
            .into_iter()
            .filter(|(m, _)| m.source == node)
            .filter_map(|(m, _)| m.moved_at)
            .filter(|at| *at + CLOCK_MARGIN > sample.interval_start)
            .min();
        Ok(ended.is_some_and(|at| sample.end() > at + CLOCK_MARGIN))
    }

    /// Takes `sample` from `node`; the effect of not believing it, where it is not believed.
    pub(super) async fn metering(
        &self,
        tx: &mut S::Tx,
        node: Uuid,
        sample: &Sample,
    ) -> Result<Option<Effect>, AppError> {
        let w = sample.workload;
        if let Some(why) = implausible(sample, self.clock.now()) {
            let mut e = rejected(why, "metering");
            e.workload = Some(w);
            return Ok(Some(e));
        }
        // A node that holds the workload's lease now names no epoch past it: that is an epoch
        // never issued to it.
        let current = self.store.lease_epoch(w, node).await?;
        if current.is_some_and(|epoch| sample.lease_epoch > epoch) {
            let mut e = rejected("the sample names a lease epoch never issued", "metering");
            e.workload = Some(w);
            return Ok(Some(e));
        }
        if self.past_the_move(node, sample, current).await? {
            let mut e =
                rejected("the sample runs past its workload's move off the node", "metering");
            e.workload = Some(w);
            return Ok(Some(e));
        }
        if !self.store.ever_leased(w, node).await? && !self.store.nodes_of(w).await?.contains(&node)
        {
            let mut e = rejected("the node never held the workload", "metering");
            e.workload = Some(w);
            return Ok(Some(e));
        }
        let before = tx.metering_reading(w, node).await?;
        let measured = match measure(sample, before.as_ref()) {
            Ok(m) => m,
            Err(why) => {
                let mut e = rejected(&why.to_string(), "metering");
                e.workload = Some(w);
                return Ok(Some(e));
            }
        };
        let mut record = Record::measured(sample, node, &measured);
        record.supersedes = tx.metering_gaps_within(w, node, (record.from, record.to)).await?;
        if tx.metering_day_closed(day_of(record.from)).await? {
            record.kind = RecordKind::Adjustment;
        }
        tx.append_metering_record(&record, self.clock.now()).await?;
        let reading = Reading { end: sample.end(), counters: sample.counters.clone() };
        tx.keep_metering_reading(w, node, &reading).await?;
        Ok(None)
    }
}
