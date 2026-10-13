// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared sets (§3.3 of docs/01-model.md): the sets of a project, membership, and the intent a
//! set last declared for a member.

use super::StoreResult;
use crate::Page;
use korpis_core::workload::set::{DeclaredSet, DriftMode};
use std::future::Future;
use uuid::Uuid;

/// Reading declared sets.
pub trait SetRead {
    /// The set of `name` in `project`.
    fn declared_set(
        &self,
        project: Uuid,
        name: &str,
    ) -> impl Future<Output = StoreResult<Option<DeclaredSet>>> + Send;

    /// One set, by identifier.
    fn declared_set_by_id(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<DeclaredSet>>> + Send;

    /// A project's sets, in identifier order after the page's cursor.
    fn declared_sets(
        &self,
        project: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<DeclaredSet>>> + Send;

    /// The newest intent of `workload` that `set` declared: what the set's files still say.
    fn set_intent(
        &self,
        workload: Uuid,
        set: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Uuid>>> + Send;
}

/// Writing declared sets.
pub trait SetWrite {
    /// A new set. Another of the same name in the project is
    /// [`Unique::SetName`](super::Unique::SetName).
    fn insert_declared_set(
        &mut self,
        set: &DeclaredSet,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Changes a set's mode if it is still at `expected`; `false` if it moved.
    fn set_drift_mode(
        &mut self,
        set: Uuid,
        mode: DriftMode,
        expected: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Takes `workload` into `set` if it belongs to none; `false` if it belongs to one.
    fn join_set(
        &mut self,
        workload: Uuid,
        set: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
