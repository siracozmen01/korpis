// SPDX-License-Identifier: AGPL-3.0-or-later

//! The members of bound sets who delegated (§5.1 of docs/08-identity.md).

use super::StoreResult;
use korpis_core::authz::membership::MemberRecord;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading members.
pub trait MemberRead {
    /// The member whose principal key is `member` of the set `binding` binds.
    fn member(
        &self,
        binding: Uuid,
        member: &str,
    ) -> impl Future<Output = StoreResult<Option<MemberRecord>>> + Send;

    /// The members of `binding`'s set who hold, through it, a grant not revoked: the ones its
    /// re-verification asks about.
    fn members_who_delegated(
        &self,
        binding: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<MemberRecord>>> + Send;
}

/// Writing members. A member is recorded by the store as a grant naming them in its `via` is
/// inserted, unverified, and their set's binding is made due, so no way of issuing one forgets.
pub trait MemberWrite {
    /// Records what the set's re-verification said of its members at `at`: the first of
    /// `members` are in it, the second are not.
    fn record_members(
        &mut self,
        binding: Uuid,
        members: (&[String], &[String]),
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
