// SPDX-License-Identifier: AGPL-3.0-or-later

//! Device authorizations (§9.4 of docs/08-identity.md, RFC 8628): kept by the digest of their
//! device code and by their user code, decided by an account, and spent by the poll that finds
//! them approved.

use super::StoreResult;
use korpis_core::account::device_grant::DeviceAuthorization;
use korpis_core::principal::Holder;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// What a poll with a device code found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Polled {
    /// Nothing this control plane issued, or one already spent or forgotten.
    Unknown,
    /// One the poll found, locked until the transaction ends, its last poll before this one
    /// as it was; this poll is recorded.
    Found {
        /// The authorization.
        authorization: Box<DeviceAuthorization>,
        /// When it was polled before this, if it was.
        polled_before: Option<SystemTime>,
        /// Whether an approval may still be collected: the account that gave it may sign in,
        /// or the person's binding that gave it is not retired, and the session it was given
        /// from has not ended (Findings 74 and 75 of docs/23-walkthroughs.md). True for one not
        /// approved.
        honoured: bool,
        /// The session it was decided from, while that session lasts.
        from: Option<Uuid>,
    },
}

/// Reading device authorizations.
pub trait DeviceGrantRead {
    /// The device authorization whose user code is `code`, if one is kept.
    fn device_authorization(
        &self,
        code: &str,
    ) -> impl Future<Output = StoreResult<Option<DeviceAuthorization>>> + Send;

    /// Forgets at most `limit` device authorizations that expired before `now`; how many.
    fn forget_expired_device_authorizations(
        &self,
        now: SystemTime,
        limit: u32,
    ) -> impl Future<Output = StoreResult<u64>> + Send;
}

/// Writing device authorizations.
pub trait DeviceGrantWrite {
    /// Keeps `authorization`, its device code kept as `digest`; false if its user code is
    /// taken, which a caller answers by drawing another.
    fn insert_device_authorization(
        &mut self,
        authorization: &DeviceAuthorization,
        digest: &[u8; 32],
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Finds the device authorization whose device code has `digest`, locks it until the
    /// transaction ends, and records that it was polled `at`.
    fn poll_device(
        &mut self,
        digest: &[u8; 32],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Polled>> + Send;

    /// Lengthens the interval of device authorization `id` to `seconds`.
    fn slow_device(
        &mut self,
        id: Uuid,
        seconds: u32,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Decides the undecided device authorization whose user code is `code`, not expired at
    /// `at`, as the holder `decider` names `approved` it, from the session it names; false if
    /// there is none such.
    fn decide_device(
        &mut self,
        code: &str,
        decider: (Holder, Option<Uuid>),
        approved: bool,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Deletes device authorization `id`: spent, or refused for good.
    fn delete_device_authorization(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
