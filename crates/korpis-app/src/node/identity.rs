// SPDX-License-Identifier: AGPL-3.0-or-later

//! A running workload's identity (§9.5 of docs/08-identity.md): its node asks for a credential
//! on the workload's behalf, and the control plane issues one only for a workload it places on
//! that node, authoritatively, under a lease that node holds and that has not expired. The
//! node's word is only which workload asked; who that workload is, is the control plane's.
//!
//! Issuing is not an effect, as minting a capability token is not: what the credential is used
//! for is, and every such effect names the workload or extension that acted.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, CredentialWrite, LeaseWrite, PlacementWrite, RecipeSource, Store, Tx};
use korpis_core::authz::credential::{
    Secret, SecretKind, WORKLOAD_CREDENTIAL_LIFETIME, WorkloadCredential,
};
use korpis_core::principal::Principal;
use std::time::SystemTime;
use uuid::Uuid;

/// A credential just issued: the secret, shown to the node once, and what it is.
#[derive(Debug, Clone)]
pub struct IssuedCredential {
    /// The bearer secret.
    pub secret: Secret,
    /// Who it authenticates as.
    pub subject: Principal,
    /// When it stops working.
    pub expires_at: SystemTime,
}

/// Why a node was not given a credential for a workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRefusal {
    /// The workload is not placed on this node, or no longer exists.
    NotPlacedHere,
    /// The node holds no live lease for it.
    NoLease,
}

impl CredentialRefusal {
    /// The stable reason a client branches on.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::NotPlacedHere => "credential.not_placed_here",
            Self::NoLease => "credential.no_lease",
        }
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Issues `node` a credential for `workload`, or says why not.
    ///
    /// # Errors
    /// The store's, or no randomness.
    pub async fn issue_workload_credential(
        &self,
        node: Uuid,
        workload: Uuid,
    ) -> Result<Result<IssuedCredential, CredentialRefusal>, AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let placed = tx.placement_in(workload).await?;
        if placed.is_none_or(|p| p.node != node || !p.authoritative) {
            return Ok(Err(CredentialRefusal::NotPlacedHere));
        }
        let lease = tx.lease_for_update(workload).await?;
        let Some(lease) =
            lease.filter(|l| l.node == node && l.released_at.is_none() && l.grantor_expiry() > now)
        else {
            return Ok(Err(CredentialRefusal::NoLease));
        };
        let secret = Secret::generate_kind(SecretKind::Workload)
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let credential = WorkloadCredential {
            workload,
            subject: Principal::Workload(workload),
            node,
            epoch: lease.epoch,
            issued_at: now,
            expires_at: now + WORKLOAD_CREDENTIAL_LIFETIME,
        };
        tx.insert_workload_credential(&credential, &secret.digest()).await?;
        tx.commit().await?;
        Ok(Ok(IssuedCredential {
            secret,
            subject: credential.subject,
            expires_at: credential.expires_at,
        }))
    }
}
