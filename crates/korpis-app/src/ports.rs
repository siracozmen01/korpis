// SPDX-License-Identifier: AGPL-3.0-or-later

//! Ports: what the use cases need from the world outside, as traits.
//!
//! A [`Store`] answers reads and opens transactions; a [`Tx`] carries every write, so a change and
//! the effect recording it commit together or not at all. Methods return `impl Future + Send` so a
//! use case can run on any multi-threaded executor.

mod account;
mod acme;
mod address;
mod assertion;
mod assignment;
mod audit;
mod authz;
mod backup;
mod binding;
mod block;
mod breaker;
mod certificate;
mod changes;
mod clock;
mod condition;
mod device_grant;
mod domain;
mod edge;
mod edge_ingress;
mod edge_lease;
mod egress;
mod endpoint;
mod enrollment;
mod extension;
mod feasibility;
mod idempotency;
mod job;
mod journal;
mod layout;
mod lease;
mod lookup;
mod member;
mod metering;
mod node;
mod observation;
mod operation;
mod placement;
mod portal;
mod provider;
mod provider_call;
mod prune;
mod quota;
mod rate;
mod recipe;
mod recovery;
mod relay;
mod replica;
mod retire;
mod run;
mod session;
mod set;
mod sign_in;
mod ssh_key;
mod stream;
mod subscription;
mod template;
mod tenancy;
mod theme;
mod trust;
mod unique;
mod user_session;
mod volume;
mod workload;
mod zones;

pub use account::{
    AccountRead, AccountWrite, FactorVault, InPlace, PasswordFuture, Passwords, TotpFactor,
};
pub use acme::{Acme, AcmeError, AcmeFuture, Challenge, HttpOffer, Issued, Proof, Publish};
pub use address::{AddressRead, AddressRoute, AddressWrite};
pub use assertion::AssertionWrite;
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
pub use binding::{BindingRead, BindingWrite};
pub use block::{BlockRead, BlockWrite, DeviceVault, HeldDevice, SealedAttachment};
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
pub use edge::{EdgeRead, EdgeWrite};
pub use edge_ingress::{ChallengesHeld, EdgeIngressRead, EdgeIngressWrite};
pub use edge_lease::{EdgeLeaseRead, EdgeLeaseWrite};
pub use egress::{EgressAt, EgressRead, EgressWrite};
pub use endpoint::{EndpointRead, EndpointWrite};
pub use enrollment::{Enrollment, EnrollmentRead, EnrollmentWrite, VerifyKey};
pub use extension::{
    ExtensionPolicyRead, ExtensionPolicyWrite, ExtensionRead, ExtensionWrite, InvocationRead,
    InvocationWrite, SetExtensionPolicy,
};
pub use feasibility::{Answers, Feasibility, Feasible};
pub use idempotency::{IdempotencyRead, IdempotencyWrite, Recorded};
pub use job::{Job, JobEnd, JobRead, JobWrite, NewJob};
pub use journal::{JournalRead, JournalWrite};
pub use korpis_core::workload::run::RunEnd;
pub use layout::{LayoutChange, LayoutRead, LayoutWrite, SetLayout};
pub use lease::{EpochIssue, LeaseWrite};
pub use lookup::{LookupError, TxtLookup};
pub use member::{Asked, MemberRead, MemberWrite};
pub use metering::{MeteringRead, MeteringWrite, Overdue, UsageQuery};
pub use node::{NodeRead, NodeWrite};
pub use observation::{ObservationRead, ObservationWrite};
pub use operation::{OperationAt, OperationRead, OperationWrite};
pub use placement::{MigrationMoves, MigrationRead, MigrationWrite, PlacementRead, PlacementWrite};
pub use portal::{PortalRead, PortalWrite};
pub use provider::{ProviderRead, ProviderWrite};
pub use provider_call::{ProviderCallRead, ProviderCallWrite};
pub use prune::{AccessRecord, Area, Prefix, PruneRead, PruneWrite};
pub use quota::{
    Counted, Holding, Placed, PlacedHolding, PlacedQuotaWrite, QuotaRead, QuotaSetWrite, QuotaWrite,
};
pub use rate::{RateBuckets, RateWrite};
pub use recipe::{CachedRecipe, Described, Fetched, RecipeCache, RecipeSource, SourceError};
pub use recovery::{
    Advance, Advanced, IntentRecovery, PlacementRecovery, PlanRecovery, RecoveredFrom,
    RecoveryWrite, SlotRecovery,
};
pub use relay::{
    CertificateIssuer, IssuedCertificate, RelayCertificate, RelayEnrollment, RelayRead, RelaySftp,
    RelayWrite,
};
pub use replica::{ReplicaRead, ReplicaWrite};
pub use retire::RetirementWrite;
pub use run::{RunRead, RunWrite};
pub use session::{AgentVersion, NodeFacts, SessionWrite};
pub use set::{SetRead, SetWrite};
pub use sign_in::SignInWrite;
pub use ssh_key::{SshKeyRead, SshKeyWrite};
pub use stream::{Expired, Reported, StreamRead, StreamWrite};
pub use subscription::{SubscriptionRead, SubscriptionWrite};
pub use template::{TemplateRead, TemplateWrite};
pub use tenancy::{TenancyRead, TenancyWrite};
pub use theme::{SetTheme, ThemeChange, ThemeRead, ThemeWrite};
pub use trust::{PolicyChange, SetPolicy, TrustRead, TrustWrite};
pub use unique::Unique;
pub use user_session::{Presented, UserSessionRead, UserSessionWrite};
pub use volume::{SnapshotRead, SnapshotWrite, VolumeRead, VolumeWrite};
pub use workload::{CodecError, IntentCodec, WorkloadFilter, WorkloadRead, WorkloadWrite};
pub use zones::{UtcOnly, Zones};

use std::future::Future;

/// A store failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// A uniqueness rule refused the write.
    #[error("duplicate: {0:?}")]
    Duplicate(Unique),
    /// Stored data does not parse into the domain: a bug or tampering, never the caller's fault.
    #[error("stored data is malformed: {0}")]
    Corrupt(String),
    /// The transaction lost a deadlock or a serialization conflict to another and was rolled
    /// back: running it again can succeed.
    #[error("contended: {0}")]
    Contended(String),
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
    + MeteringRead
    + EgressRead
    + KeyRegistry
    + RecipeCache
    + WorkloadRead
    + QuotaRead
    + NodeRead
    + PlacementRead
    + MigrationRead
    + OperationRead
    + TrustRead
    + ThemeRead
    + LayoutRead
    + SshKeyRead
    + EnrollmentRead
    + AssignmentRead
    + JournalRead
    + ObservationRead
    + SetRead
    + ExtensionRead
    + InvocationRead
    + ExtensionPolicyRead
    + ConditionRead
    + EndpointRead
    + SubscriptionRead
    + ProviderRead
    + ProviderCallRead
    + RelayRead
    + StreamRead
    + VolumeRead
    + ReplicaRead
    + BlockRead
    + SnapshotRead
    + BackupRead
    + RunRead
    + JobRead
    + PruneRead
    + DomainRead
    + PortalRead
    + AddressRead
    + EdgeRead
    + EdgeLeaseRead
    + EdgeIngressRead
    + CertificateRead
    + TemplateRead
    + BindingRead
    + MemberRead
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

    /// Wakes, from now on, whenever a portal is made, changed, or deleted, or the certificate one
    /// is served with changes, and every minute besides.
    fn portal_changes(&self) -> Self::Wakeups;
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
    + PlacedQuotaWrite
    + QuotaSetWrite
    + NodeWrite
    + PlacementWrite
    + MigrationWrite
    + OperationWrite
    + RetirementWrite
    + TrustWrite
    + ThemeWrite
    + LayoutWrite
    + SshKeyWrite
    + EnrollmentWrite
    + SessionWrite
    + AssignmentWrite
    + EndpointWrite
    + LeaseWrite
    + JournalWrite
    + ObservationWrite
    + SetWrite
    + ExtensionWrite
    + InvocationWrite
    + ExtensionPolicyWrite
    + ConditionWrite
    + SubscriptionWrite
    + ProviderWrite
    + ProviderCallWrite
    + BreakerWrite
    + RelayWrite
    + RecoveryWrite
    + StreamWrite
    + VolumeWrite
    + ReplicaWrite
    + BlockWrite
    + SnapshotWrite
    + BackupWrite
    + RunWrite
    + JobWrite
    + PruneWrite
    + DomainWrite
    + PortalWrite
    + AddressWrite
    + AddressRoute
    + EdgeWrite
    + EdgeLeaseWrite
    + EdgeIngressWrite
    + CertificateWrite
    + TemplateWrite
    + BindingWrite
    + AssertionWrite
    + MemberWrite
    + MeteringWrite
    + EgressWrite
    + SignInWrite
    + Send
{
    /// Makes every write of the transaction durable at once.
    fn commit(self) -> impl Future<Output = StoreResult<()>> + Send;
}
