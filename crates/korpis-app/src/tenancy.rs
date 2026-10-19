// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tenancy: organizations and projects (§3.1 of docs/01-model.md), and each organization's
//! recipe trust policy (§7 of docs/09-recipes.md) and theme (§7 of docs/11-surface-web.md).

mod organizations;
mod projects;
mod suspension;
mod theme;
mod trust;

pub use organizations::CreateOrganization;
pub use projects::CreateProject;
pub use suspension::SuspendOrganization;
pub use theme::{GoverningTheme, SetTheme};
pub use trust::{Governing, SetTrustPolicy};
