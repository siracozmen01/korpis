// SPDX-License-Identifier: AGPL-3.0-or-later

//! User sessions (§9.4 of docs/08-identity.md): kept by the digest of their current refresh
//! credential, with every digest they rotated away, so a rotated credential presented again is
//! recognised as the copy it must be.

use super::StoreResult;
use korpis_core::account::session::UserSession;
use std::future::Future;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// What a presented refresh credential is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presented {
    /// The current credential of a session, which is locked until the transaction ends.
    Current {
        /// The session.
        session: UserSession,
        /// Whether its account may still sign in: it is not disabled.
        usable: bool,
    },
    /// A credential a session rotated away: presented again, it can only be a copy.
    Rotated {
        /// The session.
        session: Uuid,
        /// Its account.
        account: Uuid,
    },
    /// Nothing this control plane issued, or a session that has ended.
    Unknown,
}

/// Reading sessions.
pub trait UserSessionRead {
    /// The sessions of `account`, newest first.
    fn user_sessions(
        &self,
        account: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<UserSession>>> + Send;

    /// Session `id` of `account`, if it has not ended.
    fn user_session(
        &self,
        account: Uuid,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<UserSession>>> + Send;

    /// Forgets at most `limit` sessions ended by age or by `idle` at `now`; how many it forgot.
    fn forget_lapsed_sessions(
        &self,
        now: SystemTime,
        idle: Duration,
        limit: u32,
    ) -> impl Future<Output = StoreResult<u64>> + Send;
}

/// Writing sessions.
pub trait UserSessionWrite {
    /// Begins `session`, its refresh credential kept as `digest`.
    fn insert_user_session(
        &mut self,
        session: &UserSession,
        digest: &[u8; 32],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// What the refresh credential with `digest` is.
    fn present_refresh(
        &mut self,
        digest: &[u8; 32],
    ) -> impl Future<Output = StoreResult<Presented>> + Send;

    /// Rotates the refresh credential of session `id` from `old` to `new` at `at`, keeping `old`
    /// as rotated.
    fn rotate_refresh(
        &mut self,
        id: Uuid,
        old: &[u8; 32],
        new: &[u8; 32],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that session `id` of `account` re-authenticated `at`; false if it has none such.
    fn reauthenticated(
        &mut self,
        account: Uuid,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Ends session `id` of `account`; false if it has none such.
    fn end_user_session(
        &mut self,
        account: Uuid,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Ends every session of `account` but `keep`; how many it ended.
    fn end_other_sessions(
        &mut self,
        account: Uuid,
        keep: Option<Uuid>,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Ends every session of every account, as a restore does once its token key is rotated
    /// (§8 of docs/03-state.md); how many it ended.
    fn end_every_session(&mut self) -> impl Future<Output = StoreResult<u64>> + Send;
}
