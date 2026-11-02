// SPDX-License-Identifier: AGPL-3.0-or-later

//! The uniqueness rules a write can break.

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
    /// A name is one portal's at most.
    PortalName,
    /// No two address pools overlap.
    AddressPool,
    /// An edge group's name is unique within its organization.
    EdgeGroupName,
    /// A sign-in name names one account.
    AccountName,
    /// A passkey's credential is registered once.
    Passkey,
    /// An SSH key is registered once in the installation, by its fingerprint.
    SshKey,
    /// A live grant template's name is unique in its organization.
    TemplateName,
    /// A subject is bound by one live identity binding.
    BindingSubject,
    /// A node is under one operation that has not ended at a time.
    NodeOperation,
    /// Any other.
    Other,
}
