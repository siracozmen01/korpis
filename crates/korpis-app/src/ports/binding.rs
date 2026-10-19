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
    /// still `expected`; false otherwise.
    fn update_binding(
        &mut self,
        id: Uuid,
        expected: u64,
        terms: &BindingTerms,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records a re-verification of live binding `id`: its state and its last confirmation;
    /// false if it was retired meanwhile.
    fn record_verification(
        &mut self,
        id: Uuid,
        state: BindingState,
        last_verified_at: Option<SystemTime>,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Retires a live binding at `at`; false if it was retired already.
    fn retire_binding(
        &mut self,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
