// SPDX-License-Identifier: AGPL-3.0-or-later

//! The effect log.

use super::StoreResult;
use korpis_core::audit::Effect;
use korpis_core::audit::denial::DenialRun;
use korpis_core::audit::failure::Attempt;
use korpis_core::object::ObjectRef;
use korpis_core::principal::Principal;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// The log is ordered by ingest time and then identifier; a page resumes after this pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectCursor {
    /// The ingest time of the last entry seen.
    pub at: SystemTime,
    /// Its identifier.
    pub id: Uuid,
}

/// A filter over the log. Every set field narrows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EffectFilter {
    /// Effects concerning this workload.
    pub workload: Option<Uuid>,
    /// Effects of this plan.
    pub plan: Option<Uuid>,
    /// Effects concerning this node.
    pub node: Option<Uuid>,
    /// Ingested at or after this time.
    pub since: Option<SystemTime>,
}

/// Reading the log.
pub trait EffectRead {
    /// Entries matching `filter`, in log order after `after`, at most `limit`.
    fn effects(
        &self,
        filter: &EffectFilter,
        after: Option<EffectCursor>,
        limit: u32,
    ) -> impl Future<Output = StoreResult<Vec<Effect>>> + Send;

    /// The entry that recorded declaring `intent` of `workload`: the change a drifted member's
    /// head was made by (§6 of docs/13-surface-cli.md).
    fn declaring_effect(
        &self,
        workload: Uuid,
        intent: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Uuid>>> + Send;
}

/// Appending to the log. The log is append-only: there is no other write.
pub trait EffectWrite {
    /// Appends an entry; the store assigns its ingest time and returns it.
    fn append_effect(
        &mut self,
        effect: &Effect,
    ) -> impl Future<Output = StoreResult<SystemTime>> + Send;
}

/// What a run of identical denials is keyed by: who was refused, what, and on which object
/// (§10 of docs/10-api.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenialKey {
    /// The refused subject.
    pub actor: Principal,
    /// The action that was missing.
    pub action: String,
    /// The object it was refused on.
    pub object: ObjectRef,
}

/// Finding the runs of identical denials whose window has closed.
pub trait DenialRead {
    /// Runs opened at or before `opened_by`, at most `limit`, oldest first.
    fn denial_runs_opened_by(
        &self,
        opened_by: SystemTime,
        limit: u32,
    ) -> impl Future<Output = StoreResult<Vec<DenialKey>>> + Send;
}

/// The working state of runs of identical denials: mutable, and not the log, which receives a
/// run's repeats as one effect when its window closes.
pub trait DenialWrite {
    /// Locks the run for `key` until the transaction ends, and reads it.
    fn lock_denial_run(
        &mut self,
        key: &DenialKey,
    ) -> impl Future<Output = StoreResult<Option<DenialRun>>> + Send;

    /// Stores the run for `key`, replacing any.
    fn put_denial_run(
        &mut self,
        key: &DenialKey,
        run: &DenialRun,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes the run for `key`.
    fn drop_denial_run(&mut self, key: &DenialKey) -> impl Future<Output = StoreResult<()>> + Send;
}

/// What a run of identical failed attempts to authenticate is keyed by: where they came from,
/// what was presented, and what it named, if anything (§10 of docs/10-api.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureKey {
    /// The source, as [`korpis_core::audit::failure::source`] names it.
    pub source: String,
    /// What was presented.
    pub attempt: Attempt,
    /// What it named: the node of a `Hello`.
    pub target: Option<ObjectRef>,
}

/// Finding the runs of identical failures whose window has closed.
pub trait FailureRead {
    /// Runs opened at or before `opened_by`, at most `limit`, oldest first.
    fn failure_runs_opened_by(
        &self,
        opened_by: SystemTime,
        limit: u32,
    ) -> impl Future<Output = StoreResult<Vec<FailureKey>>> + Send;
}

/// The working state of runs of identical failures, as [`DenialWrite`] holds denials'.
pub trait FailureWrite {
    /// Locks the run for `key` until the transaction ends, and reads it.
    fn lock_failure_run(
        &mut self,
        key: &FailureKey,
    ) -> impl Future<Output = StoreResult<Option<DenialRun>>> + Send;

    /// Stores the run for `key`, replacing any.
    fn put_failure_run(
        &mut self,
        key: &FailureKey,
        run: &DenialRun,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes the run for `key`.
    fn drop_failure_run(
        &mut self,
        key: &FailureKey,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
