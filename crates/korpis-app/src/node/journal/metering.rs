// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's metering samples (§2 of docs/15-observability.md, "Measuring it, as designed"): each
//! is measured from the last sample of its workload on that node, and what it measured is
//! appended as a record, superseding every gap it covers, and an adjustment where the day it
//! begins in is closed. A sample is believed only from a node that held the workload, and it is
//! not an effect: metering is not the audit log, and the record is its own. Only a sample that
//! is not believed leaves an effect, as a rejection.

use super::rejected;
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, MeteringWrite as _, RecipeSource, Store};
use korpis_core::audit::Effect;
use korpis_core::metering::{Reading, Record, RecordKind, Sample, day_of, implausible, measure};
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
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
        // never issued to it. The cut at an epoch's end is Phase 3's, with migration.
        let current = self.store.lease_epoch(w, node).await?;
        if current.is_some_and(|epoch| sample.lease_epoch > epoch) {
            let mut e = rejected("the sample names a lease epoch never issued", "metering");
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
