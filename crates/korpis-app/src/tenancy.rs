// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tenancy: organizations and projects (§3.1 of docs/01-model.md), and each organization's
//! recipe trust policy (§7 of docs/09-recipes.md).

mod organizations;
mod projects;
mod trust;

pub use organizations::CreateOrganization;
pub use projects::CreateProject;
pub use trust::{Governing, SetTrustPolicy};
