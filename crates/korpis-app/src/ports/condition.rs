// SPDX-License-Identifier: AGPL-3.0-or-later

//! Conditions as events (§5 of docs/15-observability.md): what the control plane last decided of
//! a workload's convergence, and the condition events it writes when that changes.

use super::StoreResult;
use korpis_core::event::Condition;
use korpis_core::workload::status::Convergence;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading decided conditions.
pub trait ConditionRead {
    /// The workload's convergence as last decided; none before the first decision.
    fn decided_convergence(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Convergence>>> + Send;
}

/// Writing conditions.
pub trait ConditionWrite {
    /// Appends a condition event under its workload's key.
    fn append_condition(
        &mut self,
        condition: &Condition,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records `to` as the workload's convergence if it is still `expected` (none: never
    /// decided); `false` if another decision came first.
    fn swap_convergence(
        &mut self,
        workload: Uuid,
        expected: Option<Convergence>,
        to: Convergence,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
