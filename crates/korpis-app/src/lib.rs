// SPDX-License-Identifier: AGPL-3.0-or-later

//! The Korpis application layer: one use case per thing a client or node can ask for, and the
//! ports they need (§1 of ARCHITECTURE.md).
//!
//! A use case receives an authenticated [`Caller`] and domain values, authorizes through the one
//! authorization path (P6), performs the change, and records the effect in the same transaction.
//! It never names a database, a transport, or a wire type.

#![warn(missing_docs)]

pub mod audit;
pub mod authz;
pub mod backup;
pub mod backup_target;
pub mod caller;
pub mod control;
pub mod dns;
pub mod error;
pub mod event;
pub mod extension;
pub mod idempotency;
pub mod install;
pub mod node;
pub mod page;
pub mod ports;
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
    denial_window: std::time::Duration,
    stream_floor: std::time::Duration,
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
            denial_window: korpis_core::audit::denial::DEFAULT_WINDOW,
            stream_floor: std::time::Duration::ZERO,
        }
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

    /// Keeps repository keys in `vault`: without one, no repository can be made and no backup
    /// taken or restored.
    #[must_use]
    pub fn with_vault(mut self, vault: Arc<dyn ports::KeyVault>) -> Self {
        self.vault = Some(vault);
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

    /// Now, by the application's clock: what adapters render time-dependent state against.
    pub fn now(&self) -> std::time::SystemTime {
        self.clock.now()
    }

    /// The store, for the composition root's operational tasks.
    pub fn store(&self) -> &S {
        &self.store
    }
}
