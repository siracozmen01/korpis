// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates organizations hold for the ingress (§3.2 of docs/07-networking.md), and the
//! control plane's TLS key, which reads what is uploaded and seals every certificate's key.

use super::{SealedKey, StoreResult, VaultError};
use crate::page::Page;
use korpis_core::dns::certificate::{Certificate, Leaf};
use korpis_core::dns::domain::Route;
use korpis_core::workload::endpoint::Served;
use std::future::Future;
use uuid::Uuid;

/// Why an uploaded chain and key cannot be a certificate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// The chain is not PEM certificates, or its first does not parse.
    #[error("chain: {0}")]
    Chain(String),
    /// The key is not a PEM private key, or not the leaf's.
    #[error("key: {0}")]
    Key(String),
}

/// What reads certificates and keeps their keys: the control plane's TLS key, in its data
/// directory beside its backup key.
pub trait TlsVault: Send + Sync + std::fmt::Debug {
    /// What the leaf of `chain` says, once `key` is shown to be its key; and the chain in PEM, as
    /// it will be served, nothing but its certificates.
    fn read(&self, chain: &str, key: &str) -> Result<(Leaf, String), ReadError>;

    /// `key`, sealed to `certificate` alone.
    fn seal(&self, certificate: Uuid, key: &str) -> Result<SealedKey, VaultError>;

    /// The key of `certificate`, opened from `sealed`; wiped when dropped.
    fn open(
        &self,
        certificate: Uuid,
        sealed: &SealedKey,
    ) -> Result<zeroize::Zeroizing<String>, VaultError>;
}

/// Reading certificates.
pub trait CertificateRead {
    /// One certificate.
    fn certificate(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Certificate>>> + Send;

    /// The workloads whose routes are served with the certificate.
    fn routed_with(&self, id: Uuid) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// The certificate's key, sealed, once it holds one.
    fn sealed_key(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<SealedKey>>> + Send;

    /// An organization's certificates, a page at a time, in identifier order.
    fn certificates_of(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Certificate>>> + Send;
}

/// Writing certificates.
pub trait CertificateWrite {
    /// Records a certificate, with its key sealed where it has one.
    fn insert_certificate(
        &mut self,
        c: &Certificate,
        key: Option<&SealedKey>,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The certificate, locked until the transaction ends: `exclusive` against every other
    /// lock, as a change or a deletion takes it, or shared, as a claim naming it does, so that
    /// no certificate is deleted or replaced beneath a route being claimed to it.
    fn lock_certificate(
        &mut self,
        id: Uuid,
        exclusive: bool,
    ) -> impl Future<Output = StoreResult<Option<Certificate>>> + Send;

    /// Writes `c`'s names, chain, validity, state, renewal, and revision, and its key.
    fn replace_certificate(
        &mut self,
        c: &Certificate,
        key: &SealedKey,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes a certificate no route names.
    fn delete_certificate(&mut self, id: Uuid) -> impl Future<Output = StoreResult<()>> + Send;

    /// The certificate each of `workload`'s endpoints is served with, by endpoint name: those its
    /// routes name that hold a key.
    fn served_for(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<(String, Served)>>> + Send;

    /// The routes served with the certificate, as this transaction sees them.
    fn routes_served_by(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Route>>> + Send;
}
