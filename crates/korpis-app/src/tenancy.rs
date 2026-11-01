// SPDX-License-Identifier: AGPL-3.0-or-later

//! Tenancy: organizations and projects, and each organization's recipe trust policy, theme, and
//! layout.

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
