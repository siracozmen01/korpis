// SPDX-License-Identifier: AGPL-3.0-or-later

//! Certificates organizations hold for the ingress (§3.2 of docs/07-networking.md), and the
//! control plane's TLS key, which reads what is uploaded and seals every certificate's key.

use super::HttpOffer;
use super::{SealedKey, StoreResult, VaultError};
use crate::page::Page;
use korpis_core::dns::certificate::{Certificate, ExpiryAlert, HttpChallenge, Leaf, Owner, Source};
use korpis_core::dns::domain::Route;
use korpis_core::event::Severity;
use korpis_core::workload::endpoint::Served;
use std::future::Future;
use std::time::SystemTime;
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

/// An issued certificate's validity, and the expiry alert last reported for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expiry {
    /// The certificate.
    pub certificate: Uuid,
    /// Its organization.
    pub organization: Uuid,
    /// The names it covers.
    pub names: Vec<String>,
    /// Where it came from.
    pub source: Source,
    /// Valid from.
    pub not_before: SystemTime,
    /// Valid until.
    pub not_after: SystemTime,
    /// The alert last reported, if any.
    pub alerted: Option<Severity>,
}

/// A node given a certificate's key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposure {
    /// The certificate.
    pub certificate: Uuid,
    /// The node.
    pub node: Uuid,
    /// The revision it was given.
    pub revision: i64,
    /// The key's identifier.
    pub key_id: String,
    /// The chain it was given, which revoking it presents.
    pub chain: String,
    /// When.
    pub at: SystemTime,
}

/// A chain given to a node removed as compromised, waiting to be revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRevocation {
    /// The certificate.
    pub certificate: Uuid,
    /// The node.
    pub node: Uuid,
    /// The revision.
    pub revision: i64,
    /// The chain.
    pub chain: String,
}

/// A DNS-01 challenge record published for a certificate and not yet withdrawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingRecord {
    /// The certificate.
    pub certificate: Uuid,
    /// Its organization, as whom the provider is called.
    pub organization: Uuid,
    /// The DNS provider, by name.
    pub provider: String,
    /// The owner the record was applied for.
    pub owner: String,
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

    /// Every issued certificate's validity, with the expiry alert last reported for it.
    fn certificate_expiries(&self) -> impl Future<Output = StoreResult<Vec<Expiry>>> + Send;

    /// The certificates `workload`'s routes are served with, each with its endpoint's name.
    fn routed_certificates(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<(String, Uuid)>>> + Send;
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

    /// Writes `c`'s names, chain, validity, state, renewal, revision, and attempts, and its key;
    /// removes its challenges and releases it.
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

    /// The `acme` certificate of `owner` for `names`, sorted, made pending if it has none, and
    /// locked against removal until the transaction ends.
    fn acme_certificate(
        &mut self,
        organization: Uuid,
        owner: &Owner,
        names: &[String],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Uuid>> + Send;

    /// The HTTP-01 challenges standing for the certificates `workload`'s routes name.
    fn challenges_for(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<HttpChallenge>>> + Send;

    /// Makes `challenges`, each a name's HTTP-01 challenge, the certificate's, replacing any it
    /// had.
    fn put_challenges(
        &mut self,
        certificate: Uuid,
        challenges: &[(String, HttpOffer)],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// An `acme` certificate due for issuance at `now`, held for the caller until `until`: one
    /// pending or failed whose next attempt may be made, or issued and due for renewal; none if
    /// none is, or every one that is is held already.
    fn take_due(
        &mut self,
        now: SystemTime,
        until: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<Certificate>>> + Send;

    /// Records an attempt that issued nothing: the certificate's state, reason, attempts, and
    /// next attempt as `c` says, its challenges removed, and released.
    fn record_attempt(&mut self, c: &Certificate) -> impl Future<Output = StoreResult<()>> + Send;

    /// Makes every `acme` certificate not issued due at once, as a restart does, so a change of
    /// the control plane's settings is tried without waiting out a backoff.
    fn rearm_certificates(&mut self) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Removes the `acme` certificates no route names and nothing holds; how many went.
    fn remove_orphans(&mut self) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Records `alert` and appends its event, if the alert last reported for the certificate is
    /// still `expected` and its validity still ends at the alert's; whether it did.
    fn alert_certificate(
        &mut self,
        alert: &ExpiryAlert,
        expected: Option<Severity>,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records that a node was given a certificate's key.
    fn record_exposure(
        &mut self,
        exposure: &Exposure,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Marks `node` removed as compromised at `at`, and each certificate whose current key it was
    /// given exposed, an `acme` one due for a new key at once, each told to its organization as a
    /// `certificate.exposed` event; how many.
    fn expose_certificates(
        &mut self,
        node: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// The chains of `acme` certificates given to nodes removed as compromised and not yet
    /// revoked.
    fn pending_revocations(
        &mut self,
    ) -> impl Future<Output = StoreResult<Vec<PendingRevocation>>> + Send;

    /// Records what revoking `revocation` came to.
    fn record_revocation(
        &mut self,
        revocation: &PendingRevocation,
        outcome: &str,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that a challenge record for the certificate was asked of the DNS provider
    /// `provider`, applied for `owner`.
    fn put_record(
        &mut self,
        certificate: Uuid,
        provider: &str,
        owner: &str,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Forgets a challenge record, withdrawn or never published.
    fn drop_record(
        &mut self,
        certificate: Uuid,
        provider: &str,
        owner: &str,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The challenge records standing: of the certificate `certificate` names, or, without one,
    /// of every certificate no issuer holds at `now`.
    fn standing_records(
        &mut self,
        certificate: Option<Uuid>,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<StandingRecord>>> + Send;

    /// The routes served with the certificate, as this transaction sees them.
    fn routes_served_by(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Route>>> + Send;
}
