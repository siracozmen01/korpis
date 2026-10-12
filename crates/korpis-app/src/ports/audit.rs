// SPDX-License-Identifier: AGPL-3.0-or-later

//! The effect log.

use super::StoreResult;
use korpis_core::audit::Effect;
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
}

/// Appending to the log. The log is append-only: there is no other write.
pub trait EffectWrite {
    /// Appends an entry; the store assigns its ingest time and returns it.
    fn append_effect(
        &mut self,
        effect: &Effect,
    ) -> impl Future<Output = StoreResult<SystemTime>> + Send;
}
