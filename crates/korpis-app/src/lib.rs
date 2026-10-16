// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis application layer: one use case per thing a client or node can ask for, and the
//! ports they need (§1 of ARCHITECTURE.md).
//!
//! A use case receives an authenticated [`Caller`] and domain values, authorizes through the one
//! authorization path (P6), performs the change, and records the effect in the same transaction.
//! It never names a database, a transport, or a wire type.

#![warn(missing_docs)]

pub mod account;
pub mod audit;
pub mod authz;
pub mod backup;
pub mod backup_target;
pub mod caller;
pub mod certificate;
pub mod control;
pub mod dns;
pub mod domain;
pub mod error;
pub mod event;
pub mod extension;
pub mod idempotency;
pub mod install;
pub mod node;
pub mod page;
pub mod ports;
pub mod rate;
pub mod recipe;
pub mod relay;
pub mod step;
pub mod tenancy;
pub mod volume;
pub mod workload;

pub use caller::Caller;
pub use error::AppError;
pub use idempotency::{Idempotent, Reply};
pub use page::{Page, Paged};
pub use recipe::NoRecipes;

use korpis_core::authz::token::SigningKey;
use korpis_core::schedule::PlacementPolicy;
use ports::{Clock, RecipeSource, Store, SystemClock};
use std::sync::Arc;

/// The application: every use case is a method on it.
#[derive(Debug)]
pub struct App<S, C = SystemClock, R = NoRecipes> {
    store: S,
    clock: C,
    token_key: Arc<SigningKey>,
    recipes: R,
    policy: PlacementPolicy,
    session_terms: node::SessionTerms,
    assignment_sealer: Option<Arc<dyn ports::AssignmentSealer>>,
    certificate_issuer: Option<Arc<dyn ports::CertificateIssuer>>,
    vault: Option<Arc<dyn ports::KeyVault>>,
    feasibility: Option<Arc<dyn ports::Feasibility>>,
    lookup: Option<Arc<dyn ports::TxtLookup>>,
    tls: Option<Arc<dyn ports::TlsVault>>,
    acme: Option<Arc<dyn ports::Acme>>,
    timer: Option<Arc<dyn ports::Timer>>,
    denial_window: std::time::Duration,
    stream_floor: std::time::Duration,
    limiter: rate::Limiter,
    relying_party: Option<korpis_core::webauthn::RelyingParty>,
    factor_vault: Option<Arc<dyn ports::FactorVault>>,
    passwords: Arc<dyn ports::Passwords>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The application over `store`, reading time from `clock`, minting capability tokens with
    /// `token_key`, and resolving recipes from `recipes`.
    pub fn new(store: S, clock: C, token_key: Arc<SigningKey>, recipes: R) -> Self {
        let policy = PlacementPolicy::default();
        Self {
            store,
            clock,
            token_key,
            recipes,
            policy,
            session_terms: node::SessionTerms::default(),
            assignment_sealer: None,
            certificate_issuer: None,
            vault: None,
            feasibility: None,
            lookup: None,
            tls: None,
            acme: None,
            timer: None,
            denial_window: korpis_core::audit::denial::DEFAULT_WINDOW,
            stream_floor: std::time::Duration::ZERO,
            limiter: rate::Limiter::default(),
            relying_party: None,
            factor_vault: None,
            passwords: Arc::new(ports::InPlace),
        }
    }

    /// Charges requests at `rates` (§10 of docs/10-api.md) instead of the defaults.
    #[must_use]
    pub fn with_rates(mut self, rates: korpis_core::rate::Rates) -> Self {
        self.limiter = rate::Limiter::new(rates);
        self
    }

    /// Places workloads by `policy`, the installation's (§2.2 of docs/05-scheduling.md),
    /// instead of the default.
    #[must_use]
    pub fn with_placement_policy(mut self, policy: PlacementPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Seals assignments with `sealer`: without one, no node can be told anything.
    #[must_use]
    pub fn with_assignment_sealer(mut self, sealer: Arc<dyn ports::AssignmentSealer>) -> Self {
        self.assignment_sealer = Some(sealer);
        self
    }

    /// Issues relays' certificates with `issuer`: without one, no relay can join.
    #[must_use]
    pub fn with_certificate_issuer(mut self, issuer: Arc<dyn ports::CertificateIssuer>) -> Self {
        self.certificate_issuer = Some(issuer);
        self
    }

    /// Reads certificates and seals their keys with `tls` (§3.2 of docs/07-networking.md):
    /// without it, no certificate can be uploaded or issued.
    #[must_use]
    pub fn with_tls(mut self, tls: Arc<dyn ports::TlsVault>) -> Self {
        self.tls = Some(tls);
        self
    }

    /// Orders `acme` certificates from the directory `acme` reaches (§3.2 of
    /// docs/07-networking.md): without it, none is issued.
    #[must_use]
    pub fn with_acme(mut self, acme: Arc<dyn ports::Acme>) -> Self {
        self.acme = Some(acme);
        self
    }

    /// Waits with `timer`, where a use case must: the issuer of certificates, for the nodes to
    /// hold an order's challenges.
    #[must_use]
    pub fn with_timer(mut self, timer: Arc<dyn ports::Timer>) -> Self {
        self.timer = Some(timer);
        self
    }

    /// Verifies domains through `lookup` (§3.2 of docs/07-networking.md): without it, a domain is
    /// verified only by delegation or attestation.
    #[must_use]
    pub fn with_lookup(mut self, lookup: Arc<dyn ports::TxtLookup>) -> Self {
        self.lookup = Some(lookup);
        self
    }

    /// Keeps repository keys in `vault`: without one, no repository can be made and no backup
    /// taken or restored.
    #[must_use]
    pub fn with_vault(mut self, vault: Arc<dyn ports::KeyVault>) -> Self {
        self.vault = Some(vault);
        self
    }

    /// Asks the shortlist's nodes through `feasibility` before binding (§2.1 of
    /// docs/05-scheduling.md): without it, the bound node's `Prepare` alone refuses.
    #[must_use]
    pub fn with_feasibility(mut self, feasibility: Arc<dyn ports::Feasibility>) -> Self {
        self.feasibility = Some(feasibility);
        self
    }

    /// Runs agent sessions on `terms` instead of the defaults.
    #[must_use]
    pub fn with_session_terms(mut self, terms: node::SessionTerms) -> Self {
        self.session_terms = terms;
        self
    }

    /// Coalesces repeated identical denials over `window` instead of the default (§10 of
    /// docs/10-api.md).
    #[must_use]
    pub fn with_denial_window(mut self, window: std::time::Duration) -> Self {
        self.denial_window = window;
        self
    }

    /// Keeps offloaded stream segments at least `floor`, whatever a workload declares: the
    /// operator's retention floor, disclosed to every tenant with the stream (§8 of
    /// docs/17-security.md). None by default.
    #[must_use]
    pub const fn with_stream_floor(mut self, floor: std::time::Duration) -> Self {
        self.stream_floor = floor;
        self
    }

    /// Registers and verifies passkeys as `rp` (§9.6 of docs/08-identity.md): without it, no
    /// passkey can be registered or signed in with.
    #[must_use]
    pub fn with_relying_party(mut self, rp: korpis_core::webauthn::RelyingParty) -> Self {
        self.relying_party = Some(rp);
        self
    }

    /// Seals TOTP secrets with `vault`: without one, no TOTP factor can be enrolled or checked.
    #[must_use]
    pub fn with_factor_vault(mut self, vault: Arc<dyn ports::FactorVault>) -> Self {
        self.factor_vault = Some(vault);
        self
    }

    /// Hashes and checks passwords with `passwords` instead of in place.
    #[must_use]
    pub fn with_passwords(mut self, passwords: Arc<dyn ports::Passwords>) -> Self {
        self.passwords = passwords;
        self
    }

    /// The site passkeys are bound to, if one is configured: what an authenticator app names
    /// the installation by.
    pub fn passkey_site(&self) -> Option<&str> {
        self.relying_party.as_ref().map(|rp| rp.id.as_str())
    }

    /// Now, by the application's clock: what adapters render time-dependent state against.
    pub fn now(&self) -> std::time::SystemTime {
        self.clock.now()
    }

    /// The store, for the composition root's operational tasks.
    pub fn store(&self) -> &S {
        &self.store
    }
}
