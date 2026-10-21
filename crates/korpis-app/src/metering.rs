// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keeping metering honest between samples (§2 of docs/15-observability.md): an interval of a
//! workload a node holds that no record covers two intervals after it ended is written as a gap,
//! one record an interval, which a measurement arriving later supersedes; and a UTC day closes
//! once its lateness window passes, after which a record of it is an adjustment.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, MeteringWrite as _, RecipeSource, Store, Tx as _};
use korpis_core::metering::{Record, day_of, gaps, interval_of};

mod egress;
mod usage;

pub use egress::{Reckoned, reckon};
pub use usage::{UsageCursor, UsageOf, UsageWindow};

/// The most gaps written for one workload on one node in one pass, so a node silent for months
/// is caught up over several passes rather than in one transaction.
const MOST_GAPS: usize = 24 * 7;

/// What one pass kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Kept {
    /// Gaps written.
    pub gaps: usize,
    /// Days closed.
    pub closed: u64,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Writes every gap now due and closes every day whose lateness window has passed.
    pub async fn keep_metering(&self) -> Result<Kept, AppError> {
        let now = self.clock.now();
        let interval = self.session_terms.metering_interval;
        let cutoff = interval_of(now, interval).checked_sub(interval * 2).unwrap_or(now);
        let mut kept = Kept::default();
        for o in self.store.metering_overdue(cutoff).await? {
            let spans: Vec<_> =
                gaps(o.from, cutoff, interval).into_iter().take(MOST_GAPS).collect();
            let Some(through) = spans.last().map(|(_, to)| *to) else { continue };
            let mut tx = self.store.begin().await?;
            for span in &spans {
                tx.append_metering_record(&Record::gap(o.workload, o.node, *span), now).await?;
            }
            tx.mark_metering_gapped(o.workload, o.node, (o.from, through)).await?;
            tx.commit().await?;
            kept.gaps += spans.len();
        }
        let before = day_of(now.checked_sub(self.metering_lateness).unwrap_or(now));
        let mut tx = self.store.begin().await?;
        kept.closed = tx.close_metering_days(before, now).await?;
        tx.commit().await?;
        Ok(kept)
    }
}
