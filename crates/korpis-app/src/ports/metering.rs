// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering's records (§2 of docs/15-observability.md, "Measuring it, as designed").

use super::StoreResult;
use korpis_core::metering::{Reading, Record};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A workload on a node it is placed on whose last sample, or last gap, ends before a cutoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overdue {
    /// The workload.
    pub workload: Uuid,
    /// The node.
    pub node: Uuid,
    /// Where nothing covers it from.
    pub from: SystemTime,
}

/// Which records to read: of one workload, a project's, or an organization's subtree's, whose
/// intervals begin in a window, after a page's last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageQuery {
    /// One workload's.
    pub workload: Option<Uuid>,
    /// Every workload of a project.
    pub project: Option<Uuid>,
    /// Every workload of an organization's subtree.
    pub organization: Option<Uuid>,
    /// Intervals beginning at or after this.
    pub from: SystemTime,
    /// And before this.
    pub to: SystemTime,
    /// Only the records nothing later supersedes.
    pub current: bool,
    /// Each record in the day it counts in, as an invoice sums them: `from` and `to` are the
    /// starts of UTC days, a measurement or a gap counts in the day its interval begins and an
    /// adjustment in the day it was recorded, and, with `current`, a record superseded only by
    /// an adjustment is still read in its closed day.
    pub by_period: bool,
    /// After this interval start and record, in their order.
    pub after: Option<(SystemTime, Uuid)>,
    /// At most this many.
    pub limit: u32,
}

/// Reading metering.
pub trait MeteringRead {
    /// The records `query` asks for, in the order their intervals begin.
    fn metering_usage(
        &self,
        query: &UsageQuery,
    ) -> impl Future<Output = StoreResult<Vec<Record>>> + Send;

    /// The days closed from `from` to before `to`: each day's start and when it closed.
    fn metering_closed_days(
        &self,
        from: SystemTime,
        to: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<(SystemTime, SystemTime)>>> + Send;

    /// Every workload, on each node it is placed on, that nothing covers from before `cutoff`:
    /// from its last sample or gap there, or from its placement where it has none.
    fn metering_overdue(
        &self,
        cutoff: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Overdue>>> + Send;

    /// The records of `workload` whose intervals begin from `from` and before `to`, in the order
    /// they begin, every version included.
    fn metering_records(
        &self,
        workload: Uuid,
        from: SystemTime,
        to: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Record>>> + Send;
}

/// Writing metering; every method runs in the transaction that ingests a node's journal.
pub trait MeteringWrite {
    /// The last sample taken of `workload` on `node`, locked until the transaction ends.
    fn metering_reading(
        &mut self,
        workload: Uuid,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Reading>>> + Send;

    /// Keeps `reading` as the last sample taken of `workload` on `node`.
    fn keep_metering_reading(
        &mut self,
        workload: Uuid,
        node: Uuid,
        reading: &Reading,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The gaps of `workload` on `node` overlapping `span` that nothing supersedes yet.
    fn metering_gaps_within(
        &mut self,
        workload: Uuid,
        node: Uuid,
        span: (SystemTime, SystemTime),
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Whether the UTC day `day` begins is closed.
    fn metering_day_closed(
        &mut self,
        day: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Gaps of `workload` on `node` are written from `from` through `through`.
    fn mark_metering_gapped(
        &mut self,
        workload: Uuid,
        node: Uuid,
        span: (SystemTime, SystemTime),
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Closes, at `now`, every day before the one `before` begins that is not closed, from the
    /// day after the last closed, or else the first day any record covers; how many.
    fn close_metering_days(
        &mut self,
        before: SystemTime,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Appends `record`, recorded at `at`.
    fn append_metering_record(
        &mut self,
        record: &Record,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
