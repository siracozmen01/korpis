// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ports: what the use cases need from the world outside, as traits (§1 of ARCHITECTURE.md).
//!
//! A [`Store`] answers reads and opens transactions; a [`Tx`] carries every write, so a change
//! and the effect recording it commit together or not at all (§4 of docs/03-state.md). Methods
//! return `impl Future + Send` so a use case can run on any multi-threaded executor.

mod audit;
mod authz;
mod clock;
mod idempotency;
mod node;
mod placement;
mod quota;
mod recipe;
mod tenancy;
mod workload;

pub use audit::{EffectCursor, EffectFilter, EffectRead, EffectWrite};
pub use authz::{
    CredentialRead, CredentialWrite, GrantRead, GrantWrite, HierarchyRead, KeyRegistry,
};
pub use clock::{Clock, SystemClock};
pub use idempotency::{IdempotencyRead, IdempotencyWrite, Recorded};
pub use node::{NodeRead, NodeWrite};
pub use placement::{PlacementRead, PlacementWrite};
pub use quota::{Holder, QuotaWrite};
pub use recipe::{CachedRecipe, Fetched, RecipeCache, RecipeSource, SourceError};
pub use tenancy::{TenancyRead, TenancyWrite};
pub use workload::{CodecError, IntentCodec, WorkloadFilter, WorkloadRead, WorkloadWrite};

use std::future::Future;

/// Uniqueness rules a write can break, by what they mean rather than what they are called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unique {
    /// Organization names are unique among siblings.
    OrganizationName,
    /// Project names are unique within an organization.
    ProjectName,
    /// Workload names are unique within a project among workloads not deleted.
    WorkloadName,
    /// One response per caller and idempotency key.
    IdempotencyKey,
    /// Any other.
    Other,
}

/// A store failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// A uniqueness rule refused the write.
    #[error("duplicate: {0:?}")]
    Duplicate(Unique),
    /// Stored data does not parse into the domain: a bug or tampering, never the caller's fault.
    #[error("stored data is malformed: {0}")]
    Corrupt(String),
    /// Anything else: connectivity, timeouts, the database refusing.
    #[error("{0}")]
    Failed(String),
}

/// The result of a store call.
pub type StoreResult<T> = Result<T, StoreError>;

/// The read side of the store, and where transactions begin.
pub trait Store:
    TenancyRead
    + HierarchyRead
    + GrantRead
    + CredentialRead
    + EffectRead
    + IdempotencyRead
    + KeyRegistry
    + RecipeCache
    + WorkloadRead
    + NodeRead
    + PlacementRead
    + Send
    + Sync
    + 'static
{
    /// A transaction.
    type Tx: Tx;

    /// Opens a transaction.
    fn begin(&self) -> impl Future<Output = StoreResult<Self::Tx>> + Send;
}

/// A transaction: every write goes through one. Dropped without [`Tx::commit`], it rolls back.
pub trait Tx:
    TenancyWrite
    + GrantWrite
    + CredentialWrite
    + EffectWrite
    + IdempotencyWrite
    + WorkloadWrite
    + QuotaWrite
    + NodeWrite
    + PlacementWrite
    + Send
{
    /// Makes every write of the transaction durable at once.
    fn commit(self) -> impl Future<Output = StoreResult<()>> + Send;
}
