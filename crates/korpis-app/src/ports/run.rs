// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs, and what the control plane keeps of each workload that has them (§4 of
//! docs/05-scheduling.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::workload::run::{Run, RunEnd, RunState};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading runs.
pub trait RunRead {
    /// A run.
    fn run(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Run>>> + Send;

    /// A workload's runs, newest first, after the page's cursor.
    fn runs(
        &self,
        workload: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Run>>> + Send;

    /// A workload's active runs, oldest first.
    fn active_runs(&self, workload: Uuid) -> impl Future<Output = StoreResult<Vec<Run>>> + Send;

    /// The workloads whose runs the control plane tends: every live one whose target is a task
    /// or scheduled workload, and every one with an active run.
    fn workloads_with_runs(&self) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// What is kept of a workload's runs beside them.
    fn run_state(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<RunState>>> + Send;
}

/// Writing runs, inside a transaction.
pub trait RunWrite {
    /// What is kept of a workload's runs, made empty if there is none, locked until the
    /// transaction ends: what two control planes making the workload's runs take in turn.
    fn lock_run_state(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<RunState>> + Send;

    /// A workload's active runs, oldest first, as the transaction sees them.
    fn active_runs_within(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Run>>> + Send;

    /// A workload's newest run of any status, as the transaction sees it.
    fn newest_run_within(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Run>>> + Send;

    /// Run `id`, as this transaction sees it: with what it changed earlier, which a journal
    /// batch reporting a run's start and then its segments relies on.
    fn run_within(&mut self, id: Uuid) -> impl Future<Output = StoreResult<Option<Run>>> + Send;

    /// Every run of the workload the store still holds, in any status, oldest first: what a
    /// node keeps the streams of, since a run retention has not deleted may still be read, and
    /// one the node has not yet heard ended may still be running.
    fn kept_runs_within(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Keeps `state`.
    fn set_run_state(&mut self, state: &RunState) -> impl Future<Output = StoreResult<()>> + Send;

    /// Adds `run`; false, adding nothing, when a run for the same occurrence or power cycle of
    /// the workload exists.
    fn insert_run(&mut self, run: &Run) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records that `node` started the pending run `id` at `at`; false when it is not pending.
    fn start_run(
        &mut self,
        id: Uuid,
        node: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Ends the active run `id` as `end` says; false when it is not active.
    fn end_run(&mut self, id: Uuid, end: RunEnd) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Deletes a workload's ended runs beyond the newest `keep_successful` succeeded ones and the
    /// newest `keep_failed` that ended any other way; the runs deleted.
    fn prune_runs(
        &mut self,
        workload: Uuid,
        keep_successful: u32,
        keep_failed: u32,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;
}
