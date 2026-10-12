// SPDX-License-Identifier: AGPL-3.0-or-later

//! Database errors, translated into what the application can act on.

use korpis_app::ports::{StoreError, Unique};

/// The uniqueness rule a constraint enforces, by constraint name.
fn unique(constraint: Option<&str>) -> Unique {
    match constraint {
        Some("organization_name_in_parent") => Unique::OrganizationName,
        Some("project_organization_id_name_key") => Unique::ProjectName,
        Some("idempotency_pkey") => Unique::IdempotencyKey,
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
