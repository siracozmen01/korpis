// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tenancy: organizations and projects (§3.1 of docs/01-model.md), and each organization's
//! recipe trust policy (§7 of docs/09-recipes.md), theme (§7 of docs/11-surface-web.md), and
//! layout (question 2 of §10 of docs/11-surface-web.md).

mod ceiling;
mod landing;
mod layout;
mod organizations;
mod placement;
mod projects;
mod quota;
mod suspension;
mod theme;
mod trust;

pub use ceiling::{Ceiling, MAX_CEILING, MIN_CEILING};
pub use layout::{GoverningLayout, SetLayout};
pub use organizations::CreateOrganization;
pub use placement::PolicyAt;
pub use projects::CreateProject;
pub use quota::{QuotaSetAt, SetQuota};
pub use suspension::SuspendOrganization;
pub use theme::{GoverningTheme, SetTheme};
pub use trust::{Governing, SetTrustPolicy};
