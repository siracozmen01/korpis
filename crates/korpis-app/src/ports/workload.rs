// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads, their intents and power declarations, and their plans (§3.3 of docs/01-model.md,
//! §3 and §7 of docs/03-state.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::workload::plan::PlanStatus;
use korpis_core::workload::{Intent, IntentBody, Plan, PowerRecord, Workload};
use std::future::Future;
use uuid::Uuid;

/// Why an intent body does not encode or decode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("intent body: {0}")]
pub struct CodecError(pub String);

/// How an intent body is written to and read from the store. §3.1 of docs/03-state.md stores it
/// in the wire schema's canonical JSON, a format the store does not own and must not depend on,
/// so the composition root hands the store an implementation of this.
pub trait IntentCodec: std::fmt::Debug + Send + Sync + 'static {
    /// The schema version recorded beside every body this codec writes.
    fn schema_version(&self) -> &str;

    /// The stored form of a body, as JSON text.
    fn encode(&self, body: &IntentBody) -> Result<String, CodecError>;

    /// A body stored under `schema_version`.
    fn decode(&self, json: &str, schema_version: &str) -> Result<IntentBody, CodecError>;
}

/// Which workloads to list; every set field must match.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkloadFilter {
    /// In this project.
    pub project: Option<Uuid>,
    /// Owned by this organization.
    pub organization: Option<Uuid>,
}

/// Reading workloads and what hangs from them.
pub trait WorkloadRead {
    /// One workload, deleted or not.
    fn workload(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Workload>>> + Send;

    /// Workloads not deleted, in identifier order after the page's cursor.
    fn workloads(
        &self,
        filter: &WorkloadFilter,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Workload>>> + Send;

    /// One intent.
    fn intent(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Intent>>> + Send;

    /// A workload's intents, oldest first, after the page's cursor.
    fn intents(
        &self,
        workload: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Intent>>> + Send;

    /// A workload's current power declaration.
    fn power(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<PowerRecord>>> + Send;

    /// One plan.
    fn plan(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Plan>>> + Send;

    /// A workload's plans, oldest first, after the page's cursor.
    fn plans(
        &self,
        workload: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Plan>>> + Send;
}

/// Writing workloads. The compare-and-swaps return `false` when the state they were computed
/// against has moved, which the use case reports as a conflict (§3.2 of docs/03-state.md).
pub trait WorkloadWrite {
    /// One workload, locked until the transaction ends, so that declarations, power changes and
    /// approvals for it are serialized.
    fn lock_workload(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Workload>>> + Send;

    /// A new workload with its first intent and power declaration. A workload of the same name
    /// in the project is [`Unique::WorkloadName`](super::Unique::WorkloadName).
    fn create_workload(
        &mut self,
        workload: &Workload,
        intent: &Intent,
        power: &PowerRecord,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Appends `intent` and moves the head to it, if the head is still `intent.version - 1`.
    fn append_intent(&mut self, intent: &Intent) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Appends a power declaration, if the current one is still `version - 1`.
    fn append_power(
        &mut self,
        power: &PowerRecord,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Whether a plan of `workload` is applying: a plan approved meanwhile is queued behind it
    /// (§7 of docs/03-state.md).
    fn plan_applying(&mut self, workload: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Inserts a plan.
    fn insert_plan(&mut self, plan: &Plan) -> impl Future<Output = StoreResult<()>> + Send;

    /// One plan, locked until the transaction ends.
    fn lock_plan(&mut self, id: Uuid) -> impl Future<Output = StoreResult<Option<Plan>>> + Send;

    /// Records a plan's decision (status, approval, schedule, progress), if its status is still
    /// `from`.
    fn update_plan(
        &mut self,
        plan: &Plan,
        from: PlanStatus,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Marks every proposed plan of `workload` other than `keep` superseded, and returns how
    /// many there were: declaring a newer intent supersedes every unapproved plan for an older
    /// one (§3.3 of docs/01-model.md).
    fn supersede_proposed(
        &mut self,
        workload: Uuid,
        keep: Uuid,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Moves the target to `to`, if the target version is still `expected` and `to` is the
    /// head: only the head is approvable (§3.2 of docs/03-state.md).
    fn move_target(
        &mut self,
        workload: Uuid,
        to: Uuid,
        expected: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
