// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sign-ins through an identity provider, begun and not yet completed (§9.7 of
//! docs/08-identity.md).

use super::StoreResult;
use korpis_core::authz::sign_in::PendingSignIn;
use std::future::Future;
use std::time::SystemTime;

/// Writing pending sign-ins.
pub trait SignInWrite {
    /// Keeps `pending`, found by `state_digest`.
    fn insert_pending_sign_in(
        &mut self,
        state_digest: &[u8; 32],
        pending: &PendingSignIn,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Spends the sign-in `state_digest` finds, if it is unexpired at `now`: it is gone after,
    /// so a state completes one sign-in at most.
    fn spend_pending_sign_in(
        &mut self,
        state_digest: &[u8; 32],
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<PendingSignIn>>> + Send;

    /// Forgets the sign-ins that expired before `before`, unspent; how many.
    fn forget_pending_sign_ins(
        &mut self,
        before: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;
}
