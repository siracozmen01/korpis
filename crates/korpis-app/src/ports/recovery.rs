// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recovering what a restored store lost from the assignments nodes present (§8 of
//! docs/03-state.md): rows written as recovered, each saying which node presented it and which
//! signature vouched for it, and the workload's counters moved forward past what was presented.

use super::StoreResult;
use korpis_core::workload::{Intent, PowerRecord, Workload};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Where a recovered row came from: kept with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredFrom {
    /// The node that presented the assignment.
    pub node: Uuid,
    /// The assignment key that signed it.
    pub key_id: String,
    /// The signature, over the assignment's exact bytes.
    pub signature: Vec<u8>,
    /// The assignment's generation.
    pub generation: u64,
    /// When it was recovered.
    pub at: SystemTime,
}

/// What recovering an intent did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentRecovery {
    /// It was missing and is now recorded.
    Recorded,
    /// The store holds it already.
    Held,
    /// The store holds another intent at its version, which no correct history allows: nothing
    /// was written.
    VersionTaken,
}

/// How a recovered workload's counters move: each only forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advance {
    /// The workload.
    pub workload: Uuid,
    /// The presented target: the head moves to it if it is newer than the head, and the target
    /// if it is newer than the target.
    pub target: (Uuid, u64),
    /// The presented power version: the power head and power target move to it likewise.
    pub power: u64,
}

/// What advancing moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Advanced {
    /// The head moved to the presented target.
    pub head: bool,
    /// The target moved to it.
    pub target: bool,
    /// The power target moved to the presented power version.
    pub power: bool,
}

/// Writing what a restore recovers.
pub trait RecoveryWrite {
    /// Records a workload the store lacks, with no intent yet; false if its name is taken in its
    /// project by a workload not deleted.
    fn recover_workload(
        &mut self,
        workload: &Workload,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records `intent` as recovered unless the store holds it. A parent or a declared set the
    /// store does not hold is left out, since the row cannot name what is not there.
    fn recover_intent(
        &mut self,
        intent: &Intent,
        from: &RecoveredFrom,
    ) -> impl Future<Output = StoreResult<IntentRecovery>> + Send;

    /// Records `power` as recovered unless the store holds a declaration of its version; true if
    /// it did.
    fn recover_power(
        &mut self,
        power: &PowerRecord,
        from: &RecoveredFrom,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Moves the workload's head, target, power head, and power target forward to what was
    /// presented, each only if what it holds is older, and its version floor past both.
    fn advance_recovered(
        &mut self,
        advance: Advance,
    ) -> impl Future<Output = StoreResult<Advanced>> + Send;
}
