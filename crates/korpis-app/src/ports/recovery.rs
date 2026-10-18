// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recovering what a restored store lost from the assignments nodes present (§8 of
//! docs/03-state.md): rows written as recovered, each saying which node presented it and which
//! signature vouched for it, and the workload's counters moved forward past what was presented.

use super::StoreResult;
use korpis_core::placement::Placement;
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

/// What recovering a placement did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementRecovery {
    /// The workload is now placed on the presenting node; on `replaced` before, if anywhere.
    Bound {
        /// The node its authoritative placement was on.
        replaced: Option<Uuid>,
    },
    /// It was on the presenting node already.
    Held,
    /// It stays on another node: the epoch presented is not above both the store's lease epoch
    /// for the workload and the highest one presented for that node.
    Elsewhere {
        /// The node it stays on.
        node: Uuid,
    },
}

/// What recovering an id slot did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRecovery {
    /// The workload holds the slot on the node now, and held nothing else there; `taken` names
    /// a workload the store gave the slot to that is not placed on the node.
    Given {
        /// The workload it was taken from.
        taken: Option<Uuid>,
    },
    /// The store held exactly that.
    Held,
    /// The store gives the slot to `holder`, which it places on the node: nothing was changed,
    /// since taking it could give two workloads one range of host ids.
    Conflict {
        /// The workload holding it.
        holder: Uuid,
    },
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

    /// Binds the workload's authoritative placement to `placement`'s node, bound by recovery at
    /// `epoch`, the lease epoch the presented assignment names. A workload with no placement is
    /// bound there; one placed on that node keeps its placement, with `epoch` kept beside it and
    /// any hold released, since its node presented it;
    /// one placed on another node is moved only if `epoch` is above both the store's lease
    /// epoch for it and the epoch presented for the node it is on, so a replayed older
    /// assignment moves nothing. Moving it removes every placement of the workload, the record
    /// of it being unplaced, and its id slots on other nodes.
    fn recover_placement(
        &mut self,
        placement: &Placement,
        epoch: u64,
    ) -> impl Future<Output = StoreResult<PlacementRecovery>> + Send;

    /// Gives `workload` id slot `slot` on `node`, the one the node presented it running under,
    /// unless another workload the store places on that node holds it there.
    fn recover_id_slot(
        &mut self,
        node: Uuid,
        workload: Uuid,
        slot: u32,
    ) -> impl Future<Output = StoreResult<SlotRecovery>> + Send;

    /// Holds, at `at`, every authoritative placement on `node` whose workload is not deleted, not
    /// held already, and not among `presented`: the node presented its assignments without it.
    /// The workloads it held.
    fn hold_unpresented(
        &mut self,
        node: Uuid,
        presented: &[Uuid],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Releases `workload`'s held placement; the node it is on, if it was held.
    fn confirm_held(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Uuid>>> + Send;
}
