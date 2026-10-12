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
pub mod caller;
pub mod error;
pub mod idempotency;
pub mod install;
pub mod page;
pub mod ports;
pub mod tenancy;

pub use caller::Caller;
pub use error::AppError;
pub use idempotency::{Idempotent, Reply};
pub use page::{Page, Paged};

use korpis_core::authz::token::SigningKey;
use ports::{Clock, Store, SystemClock};
use std::sync::Arc;

/// The application: every use case is a method on it.
#[derive(Debug)]
pub struct App<S, C = SystemClock> {
    store: S,
    clock: C,
    token_key: Arc<SigningKey>,
}

impl<S: Store, C: Clock> App<S, C> {
    /// The application over `store`, reading time from `clock` and minting capability tokens
    /// with `token_key`.
    pub fn new(store: S, clock: C, token_key: Arc<SigningKey>) -> Self {
        Self { store, clock, token_key }
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
