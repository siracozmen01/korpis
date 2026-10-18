// SPDX-License-Identifier: AGPL-3.0-or-later

//! Relays that run as their own process (§3.1 of docs/02-architecture.md): their enrollment,
//! their record, and the certificates the installation's authority issues them.

use super::StoreResult;
use korpis_core::event::Severity;
use korpis_core::principal::Principal;
use korpis_core::relay::{CertificateAlert, Relay, RelayUrl};
use korpis_core::tenancy::Name;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// What a relay enrollment token binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEnrollment {
    /// The Token subject a relay presents.
    pub token: Uuid,
    /// The token's grant of `node.enroll` over the root organization, with `max_uses` 1.
    pub grant: Uuid,
    /// The root organization.
    pub organization: Uuid,
    /// The relay's name.
    pub relay_name: Name,
    /// Where it will be reached.
    pub relay_url: RelayUrl,
    /// Who created the token.
    pub created_by: Principal,
    /// When.
    pub created_at: SystemTime,
    /// When it may no longer be redeemed.
    pub expires_at: SystemTime,
    /// The relay that joined with it, once one has.
    pub relay: Option<Uuid>,
}

/// A joined relay's certificate, as the expiry sweep sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayCertificate {
    /// The relay, not removed.
    pub relay: Relay,
    /// The root organization it was enrolled under.
    pub organization: Uuid,
    /// When its certificate expires.
    pub expires_at: SystemTime,
    /// The last step an event reported since the certificate was issued.
    pub alerted: Option<Severity>,
}

/// Reading relays.
pub trait RelayRead {
    /// Every relay, removed ones included, by name.
    fn relays(&self) -> impl Future<Output = StoreResult<Vec<Relay>>> + Send;

    /// One relay.
    fn relay(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Relay>>> + Send;

    /// A relay's identity key, its public half.
    fn relay_key(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Vec<u8>>>> + Send;

    /// The certificate of every relay not removed that holds one.
    fn relay_certificates(&self)
    -> impl Future<Output = StoreResult<Vec<RelayCertificate>>> + Send;

    /// The relay enrollment `token` binds, if it is a relay enrollment token.
    fn relay_enrollment(
        &self,
        token: Uuid,
    ) -> impl Future<Output = StoreResult<Option<RelayEnrollment>>> + Send;
}

/// Writing relays.
pub trait RelayWrite {
    /// Records a relay enrollment token.
    fn insert_relay_enrollment(
        &mut self,
        enrollment: &RelayEnrollment,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records `relay` with its identity key as redeeming `token`; false if the token was
    /// redeemed meanwhile. A name, URL, or key already taken is a duplicate.
    fn join_relay(
        &mut self,
        relay: &Relay,
        public_key: &[u8],
        token: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records when the certificate last issued to `relay` expires, clearing any alert.
    fn record_relay_certificate(
        &mut self,
        relay: Uuid,
        expires_at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records `nonce` as seen from `relay` at `at`, forgetting that relay's nonces older than
    /// `forget_before`; false if it was already seen.
    fn remember_relay_nonce(
        &mut self,
        relay: Uuid,
        nonce: &[u8],
        at: SystemTime,
        forget_before: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records that `alert` was reported, with its event, if the relay's last report is still
    /// `expected`; false if another report or a renewal came first.
    fn alert_relay_certificate(
        &mut self,
        alert: &CertificateAlert,
        expected: Option<Severity>,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records that `relay`'s feed was served at `at`.
    fn relay_seen(
        &mut self,
        relay: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes `relay` at `at`; false if it was already removed or does not exist.
    fn remove_relay(
        &mut self,
        relay: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Holds, at `at`, every relay not removed and not held already; how many it held.
    fn hold_relays(&mut self, at: SystemTime) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Readmits `relay`; false unless it is held and not removed.
    fn readmit_relay(&mut self, relay: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;
}

/// A certificate issued under the installation's authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedCertificate {
    /// The certificate, PEM, followed by the authority's.
    pub chain_pem: String,
    /// When it expires.
    pub expires_at: SystemTime,
}

/// The installation's certificate authority (§7 of docs/18-operations.md), as far as relays
/// need it.
pub trait CertificateIssuer: Send + Sync + std::fmt::Debug {
    /// A server certificate for `host`, binding `public_key` (an uncompressed P-256 point),
    /// valid for `lifetime` from now.
    fn issue(
        &self,
        host: &str,
        public_key: &[u8],
        lifetime: std::time::Duration,
    ) -> Result<IssuedCertificate, String>;

    /// The authority's certificate, PEM.
    fn authority_pem(&self) -> String;
}
