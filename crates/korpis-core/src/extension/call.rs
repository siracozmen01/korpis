// SPDX-License-Identifier: AGPL-3.0-or-later

//! A provider call (§3.3 and §5 of docs/16-extensions.md): a request queued for one provider,
//! claimed by whichever control-plane replica holds a stream that serves it, sent down that
//! stream with a deadline, and finished by the provider's result or by the deadline.

use super::ProviderType;
use std::time::SystemTime;
use uuid::Uuid;

/// The longest a provider is waited on: no call is unbounded (§5 of docs/16-extensions.md).
pub const MAX_CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// What a call asks. Each provider type adds its own requests; a liveness check is common to all.
#[derive(Debug, Clone, PartialEq)]
pub enum CallRequest {
    /// A liveness check every provider answers.
    Ping {
        /// Echoed back, so an answer is matched to its question.
        nonce: String,
    },
    /// `dns`: make an owner's records exactly these (§3.5 of docs/07-networking.md).
    ApplyRecords {
        /// Who the records belong to.
        owner: String,
        /// Every record it should have.
        records: Vec<crate::dns::DnsRecord>,
    },
    /// `backup_target`: access to `prefix` for one job (§5.6 of docs/06-storage.md).
    GrantAccess {
        /// The repository prefix.
        prefix: String,
        /// What for.
        mode: super::AccessMode,
        /// How long.
        ttl: std::time::Duration,
    },
    /// `backup_target`: keys under `prefix` after `after`, at most `limit`.
    ListObjects {
        /// The repository prefix.
        prefix: String,
        /// The last key of the previous page; empty for the first.
        after: String,
        /// How many.
        limit: u32,
    },
    /// `backup_target`: delete these keys, with the target's own credential.
    DeleteObjects {
        /// The keys.
        keys: Vec<String>,
    },
    /// `key_management`: a new data key, wrapped by the tenant's `key` and sealed to
    /// `recipient`, for `context` (§5.5 of docs/06-storage.md).
    GenerateKey {
        /// The tenant's key, as the repository names it.
        key: String,
        /// `<repository>/<workload>`.
        context: Vec<u8>,
        /// A one-time X25519 public key.
        recipient: Vec<u8>,
    },
    /// `key_management`: the stored `wrapped` key opened by the tenant's `key` and sealed to
    /// `recipient`, for `context`.
    UnwrapKey {
        /// The tenant's key.
        key: String,
        /// `<repository>/<workload>`.
        context: Vec<u8>,
        /// A one-time X25519 public key.
        recipient: Vec<u8>,
        /// As [`CallRequest::GenerateKey`] was answered.
        wrapped: Vec<u8>,
    },
    /// `identity`: whether a set still is what it was, and which of these people are still in
    /// it (§5.1 of docs/08-identity.md).
    VerifyMembers(super::identity::MembersAsked),
    /// `identity`: who presented this evidence (§5.2 of docs/08-identity.md).
    Assert(super::identity::Evidence),
    /// `identity`, for a provider that signs people in: where to send a person (§9.7 of
    /// docs/08-identity.md).
    Authorize(super::identity::SignIn),
    /// `health`: a query probe's next round, or its verdict (§5 of docs/04-runtimes.md).
    HealthStep(super::health::HealthStep),
    /// `address_router`: make the network route an address to a node, or to none (§3.7 of
    /// docs/07-networking.md).
    RouteAddress(crate::address::Route),
    /// `block_storage`: a device for a volume (§2.1 of docs/06-storage.md).
    CreateBlockVolume(super::block::BlockCreate),
    /// `block_storage`: a device attached to one node for one lease epoch.
    AttachBlockVolume(super::block::BlockAttach),
    /// `block_storage`: a device let go of by a node.
    DetachBlockVolume {
        /// The provider's name for it.
        device: String,
        /// The node.
        node: Uuid,
    },
    /// `block_storage`: a device deleted, data and all.
    DeleteBlockVolume {
        /// The provider's name for it.
        device: String,
    },
    /// `block_storage`: what the storage says of a device.
    DescribeBlockVolume {
        /// The provider's name for it.
        device: String,
    },
    /// `step`: resolve an install step (§4 of docs/09-recipes.md).
    RunStep {
        /// The step kind, `<publisher>/<step>`.
        step: String,
        /// Its arguments, as the recipe gives them.
        arguments: crate::value::ValueMap,
        /// The workload being installed.
        workload: Uuid,
        /// The node's platform.
        platform: super::Platform,
    },
}

/// A data key sealed to a recipient: the encapsulated X25519 key, then the 32-byte key and its
/// tag (§5.5 of docs/06-storage.md).
pub const SEALED_LEN: usize = 32 + 32 + 16;

/// The most a wrapped data key may hold: a key management service's ciphertext and its own
/// framing, never a payload.
pub const MAX_WRAPPED: usize = 4096;

/// How a call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallOutcome {
    /// The answer to a [`CallRequest::Ping`].
    Pong {
        /// The nonce it was asked with.
        nonce: String,
    },
    /// The answer to [`CallRequest::GrantAccess`].
    Access(super::ObjectAccess),
    /// The answer to [`CallRequest::ListObjects`]: the objects, and whether more follow.
    Objects {
        /// The page.
        objects: Vec<super::StoredObject>,
        /// Whether more follow the last.
        more: bool,
    },
    /// The answer to [`CallRequest::DeleteObjects`].
    Deleted {
        /// How many were deleted.
        count: u32,
    },
    /// The answer to [`CallRequest::RunStep`].
    StepResult(super::StepResult),
    /// The answer to [`CallRequest::GenerateKey`] and [`CallRequest::UnwrapKey`]: the key
    /// wrapped for storage (generating only) and sealed to the recipient.
    KeyMaterial {
        /// Wrapped by the tenant's key; empty when unwrapping.
        wrapped: Vec<u8>,
        /// Sealed to the call's recipient.
        sealed: Vec<u8>,
    },
    /// The answer to [`CallRequest::ApplyRecords`]: what it changed.
    RecordsApplied {
        /// Records created.
        created: u32,
        /// Records changed.
        updated: u32,
        /// Records removed.
        deleted: u32,
    },
    /// The answer to [`CallRequest::VerifyMembers`].
    Members(super::identity::Members),
    /// The answer to [`CallRequest::Assert`].
    Asserted(super::identity::Assertion),
    /// The answer to [`CallRequest::Authorize`].
    Authorization(super::identity::Authorization),
    /// The answer to [`CallRequest::HealthStep`].
    HealthNext(super::health::HealthNext),
    /// The answer to [`CallRequest::RouteAddress`]: the network routes it so.
    AddressRouted,
    /// The answer to [`CallRequest::CreateBlockVolume`]: the provider's name for the device.
    BlockCreated {
        /// The name.
        device: String,
    },
    /// The answer to [`CallRequest::AttachBlockVolume`].
    BlockAttached(super::block::Attachment),
    /// The answer to [`CallRequest::DetachBlockVolume`].
    BlockDetached,
    /// The answer to [`CallRequest::DeleteBlockVolume`].
    BlockDeleted,
    /// The answer to [`CallRequest::DescribeBlockVolume`].
    BlockDescribed(super::block::BlockDescription),
    /// The provider, or the control plane on its behalf, failed the call.
    Failed(CallFailure),
}

impl CallRequest {
    /// Whether `outcome` answers this request: a failure answers anything, and a success only
    /// the request it is the answer to, in a form the control plane can act on. A provider
    /// answering otherwise is broken.
    #[must_use]
    pub fn answered_by(&self, outcome: &CallOutcome) -> bool {
        match (self, outcome) {
            (_, CallOutcome::Failed(_))
            | (Self::Ping { .. }, CallOutcome::Pong { .. })
            | (Self::ApplyRecords { .. }, CallOutcome::RecordsApplied { .. })
            | (Self::RouteAddress(_), CallOutcome::AddressRouted)
            | (Self::DetachBlockVolume { .. }, CallOutcome::BlockDetached)
            | (Self::DeleteBlockVolume { .. }, CallOutcome::BlockDeleted)
            | (Self::DescribeBlockVolume { .. }, CallOutcome::BlockDescribed(_)) => true,
            (Self::CreateBlockVolume(_), CallOutcome::BlockCreated { device }) => {
                super::block::is_device(device)
            }
            (Self::AttachBlockVolume(asked), CallOutcome::BlockAttached(a)) => a.answers(asked),
            (Self::RunStep { .. }, CallOutcome::StepResult(r)) => r.problem().is_none(),
            (Self::HealthStep(step), CallOutcome::HealthNext(next)) => next.answers(step),
            (Self::GrantAccess { prefix, .. }, CallOutcome::Access(a)) => {
                a.s3.key_prefix.ends_with(prefix.as_str()) && super::is_located(&a.s3)
            }
            (Self::ListObjects { prefix, limit, .. }, CallOutcome::Objects { objects, .. }) => {
                objects.len() <= usize::try_from(*limit).unwrap_or(usize::MAX)
                    && objects.iter().all(|o| super::is_beneath(&o.key, prefix))
            }
            (Self::DeleteObjects { keys }, CallOutcome::Deleted { count }) => {
                usize::try_from(*count).is_ok_and(|n| n <= keys.len())
            }
            (Self::GenerateKey { .. }, CallOutcome::KeyMaterial { wrapped, sealed }) => {
                !wrapped.is_empty() && wrapped.len() <= MAX_WRAPPED && sealed.len() == SEALED_LEN
            }
            (Self::UnwrapKey { .. }, CallOutcome::KeyMaterial { wrapped, sealed }) => {
                wrapped.is_empty() && sealed.len() == SEALED_LEN
            }
            (Self::VerifyMembers(asked), CallOutcome::Members(m)) => asked.answered_by(m),
            (Self::Assert(_), CallOutcome::Asserted(a)) => a.well_formed(),
            (Self::Authorize(_), CallOutcome::Authorization(a)) => a.well_formed(),
            _ => false,
        }
    }
}

/// A failed call, with a reason the plan or the observation shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFailure {
    /// A stable identifier, `provider.deadline` or the provider's own.
    pub reason: String,
    /// For display only.
    pub message: String,
    /// Whether asking again may succeed.
    pub retryable: bool,
}

impl CallFailure {
    /// Whether it says the provider is unwell, as its breaker counts failures: a retryable
    /// failure (its deadline, its stream, or the provider's own word), or an answer that answers
    /// nothing. A refusal of the request itself is a correct answer.
    #[must_use]
    pub fn unwell(&self) -> bool {
        self.retryable || self.reason == "provider.malformed"
    }

    /// A failure the control plane records on the provider's behalf.
    #[must_use]
    pub fn of(reason: &str, message: &str, retryable: bool) -> Self {
        Self { reason: reason.to_owned(), message: message.to_owned(), retryable }
    }

    /// No provider answered before the deadline.
    #[must_use]
    pub fn deadline() -> Self {
        Self::of("provider.deadline", "no provider answered before the deadline", true)
    }

    /// The stream the call was sent down ended before it was answered.
    #[must_use]
    pub fn stream_ended() -> Self {
        Self::of("provider.stream_ended", "the provider's stream ended before it answered", true)
    }

    /// The provider's breaker is open: it failed or was too slow lately, and is not called
    /// until it is probed again.
    #[must_use]
    pub fn circuit_open() -> Self {
        Self::of("provider.circuit_open", "the provider is failing; its breaker is open", true)
    }

    /// The provider answered with something that is not an answer to the call.
    #[must_use]
    pub fn malformed() -> Self {
        Self::of("provider.malformed", "the provider's answer does not answer the call", false)
    }

    /// Nothing provides what the call names any more.
    #[must_use]
    pub fn unknown() -> Self {
        Self::of("provider.unknown", "no installation provides this here", false)
    }
}

/// One call, as the queue holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCall {
    /// Its identifier.
    pub id: Uuid,
    /// The organization whose installation provides what it calls.
    pub organization: Uuid,
    /// The kind of provider called.
    pub kind: ProviderType,
    /// The `provides` entry called.
    pub name: String,
    /// What it asks.
    pub request: CallRequest,
    /// When the caller stops waiting.
    pub deadline: SystemTime,
    /// When it was queued.
    pub created_at: SystemTime,
    /// The registration it was sent to, and the provider version behind it, once claimed.
    pub sent: Option<(Uuid, String)>,
    /// How it ended, and when, once it has.
    pub finished: Option<(CallOutcome, SystemTime)>,
}
