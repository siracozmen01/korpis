// SPDX-License-Identifier: AGPL-3.0-or-later

//! Enrollment (§3 of docs/18-operations.md): what a token binds, and the node identity keys it
//! leads to (§7 of docs/02-architecture.md).

use super::StoreResult;
use korpis_core::labels::Labels;
use korpis_core::principal::Principal;
use korpis_core::tenancy::Name;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// An enrollment token's binding: the node it may enroll, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrollment {
    /// The Token subject an agent presents.
    pub token: Uuid,
    /// The token's grant of `node.enroll`, with `max_uses` 1.
    pub grant: Uuid,
    /// The organization the node will belong to.
    pub organization: Uuid,
    /// The node's name.
    pub node_name: Name,
    /// The node's labels.
    pub labels: Labels,
    /// Who created the token.
    pub created_by: Principal,
    /// When.
    pub created_at: SystemTime,
    /// When it may no longer be redeemed.
    pub expires_at: SystemTime,
    /// The node enrolled with it, once it has been.
    pub node: Option<Uuid>,
}

/// A public signing key the control plane signs with, verify-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyKey {
    /// As assignments and tokens name it.
    pub key_id: String,
    /// The public half, raw.
    pub public: Vec<u8>,
    /// From when it signs.
    pub not_before: SystemTime,
    /// Until when, if bounded.
    pub not_after: Option<SystemTime>,
}

/// Reading enrollments and node identities.
pub trait EnrollmentRead {
    /// The enrollment bound to the Token subject `token`.
    fn enrollment(
        &self,
        token: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Enrollment>>> + Send;

    /// A node's identity key, its public half.
    fn node_key(&self, node: Uuid) -> impl Future<Output = StoreResult<Option<Vec<u8>>>> + Send;

    /// The identity key of every node not decommissioned, by node.
    fn node_keys(&self) -> impl Future<Output = StoreResult<Vec<(Uuid, Vec<u8>)>>> + Send;

    /// The public keys of `purpose` (`assignment` or `token`), current and historical, so a
    /// verifier holds every key that ever signed what it may be shown (§8 of docs/03-state.md);
    /// never a retired one, whose signatures a restore voided.
    fn verify_keys(
        &self,
        purpose: &str,
    ) -> impl Future<Output = StoreResult<Vec<VerifyKey>>> + Send;
}

/// Recording enrollments.
pub trait EnrollmentWrite {
    /// Retires every key of `purpose` but `keep` at `at`: what it signed stops verifying, and no
    /// verifier is given it again; how many it retired.
    fn retire_signing_keys(
        &mut self,
        purpose: &str,
        keep: &str,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Records a new enrollment.
    fn insert_enrollment(
        &mut self,
        enrollment: &Enrollment,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Marks the enrollment of `token` as having enrolled `node`; false if it already had.
    fn complete_enrollment(
        &mut self,
        token: Uuid,
        node: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
