// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering's records (§2 of docs/15-observability.md, "Measuring it, as designed").

use super::StoreResult;
use korpis_core::metering::{Reading, Record};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading metering.
pub trait MeteringRead {
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

    /// Appends `record`, recorded at `at`.
    fn append_metering_record(
        &mut self,
        record: &Record,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
