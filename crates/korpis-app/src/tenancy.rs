// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tenancy: organizations and projects (§3.1 of docs/01-model.md).

mod organizations;
mod projects;

pub use organizations::CreateOrganization;
pub use projects::CreateProject;
