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
        Some("node_name_key") => Unique::NodeName,
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
        other => StoreError::Failed(other.to_string()),
    }
}
