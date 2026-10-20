// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings (§5 of docs/08-identity.md): one live binding per subject, kept when
//! retired.

use super::StoreResult;
use crate::page::Page;
use korpis_core::authz::binding::{BindingState, BindingTerms, IdentityBinding};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading bindings.
pub trait BindingRead {
    /// A binding, live or retired.
    fn binding(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<IdentityBinding>>> + Send;

    /// The live binding of the subject whose principal key is `subject`, if it is bound.
    fn binding_of(
        &self,
        subject: &str,
    ) -> impl Future<Output = StoreResult<Option<IdentityBinding>>> + Send;

    /// The bindings declared in `organization`, by identifier; only the live ones unless
    /// `retired`.
    fn bindings_in(
        &self,
        organization: Uuid,
        retired: bool,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<IdentityBinding>>> + Send;
}

/// Writing bindings. A subject already bound live is refused as
/// `Duplicate(Unique::BindingSubject)`.
pub trait BindingWrite {
    /// Inserts a binding.
    fn insert_binding(
        &mut self,
        binding: &IdentityBinding,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Replaces the terms of live binding `id` and moves it to `expected + 1`, if its version is
    /// still `expected`; false otherwise. An answer asked under the old terms is recorded no
    /// more, as if one had been recorded, and the binding is due again at `now`.
    fn update_binding(
        &mut self,
        binding: (Uuid, u64),
        terms: &BindingTerms,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records that live binding `id`'s subject ended, whatever was recorded meanwhile: it only
    /// ever moves a binding toward unverified, so it needs no count to be recorded against. It
    /// counts one more, and makes the binding due by `due_by` at the latest. The state it had,
    /// or `None` if it was retired.
    fn record_ended(
        &mut self,
        id: Uuid,
        due_by: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<BindingState>>> + Send;

    /// Records a re-verification of live binding `id` asked when it had `asked_at` recorded:
    /// its state, its last confirmation, and when it is next due, counting one more; false if
    /// it was retired, or another answer was recorded, meanwhile.
    fn record_verification(
        &mut self,
        binding: (Uuid, u64),
        state: BindingState,
        last_verified_at: Option<SystemTime>,
        due: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Claims at most `limit` live bindings due at `now`, earliest first, by making each due
    /// again only at `held_until`, so no other control plane takes them meanwhile.
    fn claim_due_bindings(
        &mut self,
        now: SystemTime,
        held_until: SystemTime,
        limit: u32,
    ) -> impl Future<Output = StoreResult<Vec<IdentityBinding>>> + Send;

    /// Retires a live binding at `at`; false if it was retired already.
    fn retire_binding(
        &mut self,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
