// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grants, credentials, signing keys, and the object hierarchy authorization evaluates against.

use super::StoreResult;
use crate::page::Page;
use korpis_core::authz::credential::{ApiToken, WorkloadCredential};
use korpis_core::authz::{GrantRecord, Node, ObjectRef};
use korpis_core::principal::Principal;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// The ancestry of objects.
pub trait HierarchyRead {
    /// The object first, then each ancestor up to its root organization, with labels; empty if
    /// the object does not exist. One snapshot.
    fn ancestry(&self, object: ObjectRef) -> impl Future<Output = StoreResult<Vec<Node>>> + Send;
}

/// Reading grants.
pub trait GrantRead {
    /// The chain of `grant`, leaf first and root last, in one snapshot; empty if it does not
    /// exist.
    fn grant_chain(
        &self,
        grant: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<GrantRecord>>> + Send;

    /// Every chain whose leaf is held by `subject_key` and is not itself revoked. Revoked and
    /// suspended ancestors are included as they are: evaluation derives the cascade (§3.5 of
    /// docs/08-identity.md).
    fn chains_held_by(
        &self,
        subject_key: &str,
    ) -> impl Future<Output = StoreResult<Vec<Vec<GrantRecord>>>> + Send;

    /// Grants whose scope is anchored at `object`.
    fn grants_on(
        &self,
        object: ObjectRef,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<GrantRecord>>> + Send;

    /// Grants held by `subject_key`.
    fn grants_held(
        &self,
        subject_key: &str,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<GrantRecord>>> + Send;

    /// Grants revoked at or after `since`, and grants suspended at or after it whose suspension
    /// stands, sorted: the entries of the revocation list (§6.3 of docs/08-identity.md).
    fn revoked_since(
        &self,
        since: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;
}

/// Writing grants.
pub trait GrantWrite {
    /// Inserts a grant.
    fn insert_grant(&mut self, grant: &GrantRecord)
    -> impl Future<Output = StoreResult<()>> + Send;

    /// Revokes one grant at `at`. False if it was already revoked or does not exist.
    fn revoke_grant(
        &mut self,
        grant: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// How many grants descend from `grant`, for reporting what a revocation cascaded to.
    fn descendant_count(&mut self, grant: Uuid) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Takes `n` uses from a grant limited to `max` if it still has them: the compare-and-swap
    /// that makes redemption and attenuation race-free (§6.2 of docs/08-identity.md).
    fn take_uses(
        &mut self,
        grant: Uuid,
        n: u64,
        max: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}

/// Authenticating API tokens.
pub trait CredentialRead {
    /// The live token whose secret has `digest`, marking it used.
    fn authenticate(
        &self,
        digest: &[u8; 32],
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<ApiToken>>> + Send;

    /// The live workload credential whose secret has `digest`, if its workload is still placed,
    /// authoritatively, on the node it was issued through (§9.5 of docs/08-identity.md).
    fn authenticate_workload(
        &self,
        digest: &[u8; 32],
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<WorkloadCredential>>> + Send;
}

/// Creating API tokens.
pub trait CredentialWrite {
    /// Stores a token by the digest of its secret.
    fn insert_token(
        &mut self,
        token: &ApiToken,
        digest: &[u8; 32],
        created_by: &Principal,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Stores a workload credential by the digest of its secret, and forgets the ones that have
    /// expired.
    fn insert_workload_credential(
        &mut self,
        credential: &WorkloadCredential,
        digest: &[u8; 32],
    ) -> impl Future<Output = StoreResult<()>> + Send;
}

/// The public halves of the control plane's signing keys. The private halves never reach the
/// store (§6 of docs/18-operations.md).
pub trait KeyRegistry {
    /// Records a public key; recording the same key again does nothing.
    fn register_key(
        &self,
        key_id: &str,
        purpose: &str,
        public: &[u8],
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
