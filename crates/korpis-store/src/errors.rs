// SPDX-License-Identifier: AGPL-3.0-or-later

//! Database errors, translated into what the application can act on.

use korpis_app::ports::{StoreError, Unique};

/// The uniqueness rule a constraint enforces, by constraint name.
fn unique(constraint: Option<&str>) -> Unique {
    match constraint {
        Some("organization_name_in_parent") => Unique::OrganizationName,
        Some("project_organization_id_name_key") => Unique::ProjectName,
        Some("idempotency_pkey") => Unique::IdempotencyKey,
        Some("workload_name_in_project") => Unique::WorkloadName,
        Some("declared_set_name_in_project") => Unique::SetName,
        Some("extension_installed_name") => Unique::ExtensionName,
        Some("extension_provides_name_in_organization") => Unique::ProviderName,
        Some("repository_live_name") => Unique::RepositoryName,
        Some("repository_streams") => Unique::RepositoryStreams,
        Some("domain_organization_id_name_key") => Unique::DomainName,
        Some("portal_name_key") => Unique::PortalName,
        Some("node_name_key") => Unique::NodeName,
        Some("account_name_key") => Unique::AccountName,
        Some("passkey_pkey") => Unique::Passkey,
        Some("ssh_key_fingerprint_key") => Unique::SshKey,
        Some("grant_template_name") => Unique::TemplateName,
        Some("identity_binding_subject") => Unique::BindingSubject,
        Some("node_public_key_key") => Unique::NodeKey,
        Some("relay_name_key" | "relay_url_key" | "relay_public_key_key") => Unique::Relay,
        _ => Unique::Other,
    }
}

/// A database error as a store error.
pub(crate) fn from_sqlx(e: sqlx::Error) -> StoreError {
    match e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            StoreError::Duplicate(unique(d.constraint()))
        }
        // deadlock_detected and serialization_failure: the database undid this transaction so
        // another could finish, and running it again can succeed.
        sqlx::Error::Database(d) if matches!(d.code().as_deref(), Some("40P01" | "40001")) => {
            StoreError::Contended(d.message().to_owned())
        }
        other => StoreError::Failed(other.to_string()),
    }
}
