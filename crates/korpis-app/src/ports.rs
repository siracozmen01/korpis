// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ports: what the use cases need from the world outside, as traits (§1 of ARCHITECTURE.md).
//!
//! A [`Store`] answers reads and opens transactions; a [`Tx`] carries every write, so a change
//! and the effect recording it commit together or not at all (§4 of docs/03-state.md). Methods
//! return `impl Future + Send` so a use case can run on any multi-threaded executor.

mod account;
mod acme;
mod assignment;
mod audit;
mod authz;
mod backup;
mod breaker;
mod certificate;
mod changes;
mod clock;
mod condition;
mod device_grant;
mod domain;
mod endpoint;
mod enrollment;
mod extension;
mod feasibility;
mod idempotency;
mod job;
mod journal;
mod lease;
mod lookup;
mod node;
mod observation;
mod placement;
mod provider;
mod provider_call;
mod prune;
mod quota;
mod rate;
mod recipe;
mod recovery;
mod relay;
mod retire;
mod session;
mod set;
mod stream;
mod subscription;
mod tenancy;
mod theme;
mod trust;
mod user_session;
mod volume;
mod workload;

pub use account::{
    AccountRead, AccountWrite, FactorVault, InPlace, PasswordFuture, Passwords, TotpFactor,
};
pub use acme::{Acme, AcmeError, AcmeFuture, Challenge, HttpOffer, Issued, Proof, Publish};
pub use assignment::{
    AssignmentRead, AssignmentSealer, AssignmentWrite, Opened, SealError, Sealed, StoredAssignment,
};
pub use audit::{
    DenialKey, DenialRead, DenialWrite, EffectCursor, EffectFilter, EffectRead, EffectWrite,
    FailureKey, FailureRead, FailureWrite,
};
pub use authz::{
    CredentialRead, CredentialWrite, GrantRead, GrantWrite, HierarchyRead, KeyRegistry,
};
pub use backup::{BackupRead, BackupWrite, DataKey, KeyRecipient, KeyVault, SealedKey, VaultError};
pub use breaker::{BreakerWrite, ProviderKey};
pub use certificate::{
    CertificateRead, CertificateWrite, Expiry, Exposure, PendingRevocation, ReadError,
    StandingRecord, TlsVault,
};
pub use changes::{Change, Changes, Wakeups};
pub use clock::{Clock, SystemClock, Timer};
pub use condition::{ConditionRead, ConditionWrite};
pub use device_grant::{DeviceGrantRead, DeviceGrantWrite, Polled};
pub use domain::{DomainRead, DomainWrite};
pub use endpoint::{EndpointRead, EndpointWrite};
pub use enrollment::{Enrollment, EnrollmentRead, EnrollmentWrite, VerifyKey};
pub use extension::{ExtensionRead, ExtensionWrite};
pub use feasibility::{Answers, Feasibility, Feasible};
pub use idempotency::{IdempotencyRead, IdempotencyWrite, Recorded};
pub use job::{Job, JobEnd, JobRead, JobWrite, NewJob};
pub use journal::{JournalRead, JournalWrite};
pub use lease::{EpochIssue, LeaseWrite};
pub use lookup::{LookupError, TxtLookup};
pub use node::{NodeRead, NodeWrite};
pub use observation::{ObservationRead, ObservationWrite};
pub use placement::{PlacementRead, PlacementWrite};
pub use provider::{ProviderRead, ProviderWrite};
pub use provider_call::{ProviderCallRead, ProviderCallWrite};
pub use prune::{AccessRecord, Area, Prefix, PruneRead, PruneWrite};
pub use quota::{Holder, QuotaWrite};
pub use rate::{RateBuckets, RateWrite};
pub use recipe::{CachedRecipe, Described, Fetched, RecipeCache, RecipeSource, SourceError};
pub use recovery::{
    Advance, Advanced, IntentRecovery, PlacementRecovery, RecoveredFrom, RecoveryWrite,
};
pub use relay::{
    CertificateIssuer, IssuedCertificate, RelayCertificate, RelayEnrollment, RelayRead, RelayWrite,
};
pub use retire::RetirementWrite;
pub use session::{AgentVersion, NodeFacts, SessionWrite};
pub use set::{SetRead, SetWrite};
pub use stream::{Expired, Reported, StreamRead, StreamWrite};
pub use subscription::{SubscriptionRead, SubscriptionWrite};
pub use tenancy::{TenancyRead, TenancyWrite};
pub use theme::{SetTheme, ThemeChange, ThemeRead, ThemeWrite};
pub use trust::{PolicyChange, SetPolicy, TrustRead, TrustWrite};
pub use user_session::{Presented, UserSessionRead, UserSessionWrite};
pub use volume::{SnapshotRead, SnapshotWrite, VolumeRead, VolumeWrite};
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
    /// Node names are unique in the installation.
    NodeName,
    /// Relay names, URLs, and identity keys are each unique in the installation.
    Relay,
    /// A node identity key enrolls one node.
    NodeKey,
    /// One response per caller and idempotency key.
    IdempotencyKey,
    /// Declared set names are unique within a project.
    SetName,
    /// One installation of an extension's name per organization and project at a time.
    ExtensionName,
    /// A provider name is served by one installation per type in an organization.
    ProviderName,
    /// Repository names are unique within an organization among live repositories.
    RepositoryName,
    /// No two live repositories of one provider name in an organization have places one's
    /// prune could reach into: the same prefix, or one that is the other's followed by
    /// `workloads/` and more.
    RepositoryPrefix,
    /// A live repository already takes the streams of the same organization or project.
    RepositoryStreams,
    /// An organization asks for one name once.
    DomainName,
    /// A sign-in name names one account.
    AccountName,
    /// A passkey's credential is registered once.
    Passkey,
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
    RateBuckets
    + AccountRead
    + UserSessionRead
    + DeviceGrantRead
    + TenancyRead
    + HierarchyRead
    + GrantRead
    + CredentialRead
    + EffectRead
    + DenialRead
    + FailureRead
    + IdempotencyRead
    + KeyRegistry
    + RecipeCache
    + WorkloadRead
    + NodeRead
    + PlacementRead
    + TrustRead
    + ThemeRead
    + EnrollmentRead
    + AssignmentRead
    + JournalRead
    + ObservationRead
    + SetRead
    + ExtensionRead
    + ConditionRead
    + EndpointRead
    + SubscriptionRead
    + ProviderRead
    + ProviderCallRead
    + RelayRead
    + StreamRead
    + VolumeRead
    + SnapshotRead
    + BackupRead
    + JobRead
    + PruneRead
    + DomainRead
    + CertificateRead
    + Send
    + Sync
    + 'static
{
    /// A transaction.
    type Tx: Tx;

    /// A subscription to workload changes.
    type Changes: Changes;

    /// Wake-ups for event subscribers and provider calls.
    type Wakeups: Wakeups;

    /// Opens a transaction.
    fn begin(&self) -> impl Future<Output = StoreResult<Self::Tx>> + Send;

    /// Subscribes to the changes of every workload, from now on.
    fn subscribe(&self) -> Self::Changes;

    /// Wakes, from now on, whenever events are positioned.
    fn positions(&self) -> Self::Wakeups;

    /// Wakes, from now on, whenever a provider call is queued or finished.
    fn provider_calls(&self) -> Self::Wakeups;

    /// Wakes, from now on, whenever a job is queued or put back for a later attempt.
    fn jobs(&self) -> Self::Wakeups;

    /// Wakes, from now on, whenever an `acme` certificate is made, and on an interval besides,
    /// since a retry's or a renewal's time comes by the clock.
    fn certificates(&self) -> Self::Wakeups;

    /// Wakes, from now on, whenever an endpoint is placed, moved, or removed, or a node's
    /// advertised addresses change, and on an interval besides.
    fn endpoint_changes(&self) -> Self::Wakeups;
}

/// A transaction: every write goes through one. Dropped without [`Tx::commit`], it rolls back.
pub trait Tx:
    TenancyWrite
    + AccountWrite
    + UserSessionWrite
    + DeviceGrantWrite
    + RateWrite
    + GrantWrite
    + CredentialWrite
    + EffectWrite
    + DenialWrite
    + FailureWrite
    + IdempotencyWrite
    + WorkloadWrite
    + QuotaWrite
    + NodeWrite
    + PlacementWrite
    + RetirementWrite
    + TrustWrite
    + ThemeWrite
    + EnrollmentWrite
    + SessionWrite
    + AssignmentWrite
    + EndpointWrite
    + LeaseWrite
    + JournalWrite
    + ObservationWrite
    + SetWrite
    + ExtensionWrite
    + ConditionWrite
    + SubscriptionWrite
    + ProviderWrite
    + ProviderCallWrite
    + BreakerWrite
    + RelayWrite
    + RecoveryWrite
    + StreamWrite
    + VolumeWrite
    + SnapshotWrite
    + BackupWrite
    + JobWrite
    + PruneWrite
    + DomainWrite
    + CertificateWrite
    + Send
{
    /// Makes every write of the transaction durable at once.
    fn commit(self) -> impl Future<Output = StoreResult<()>> + Send;
}
