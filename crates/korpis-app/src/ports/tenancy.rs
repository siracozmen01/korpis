// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations and projects.

use super::StoreResult;
use crate::page::Page;
use korpis_core::tenancy::{Organization, OrganizationStatus, Project};
use std::future::Future;
use uuid::Uuid;

/// Reading organizations and projects.
pub trait TenancyRead {
    /// One organization.
    fn organization(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Organization>>> + Send;

    /// The organizations directly under `parent`, or the roots when it is `None`, in identifier
    /// order after the page's cursor.
    fn organizations(
        &self,
        parent: Option<Uuid>,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Organization>>> + Send;

    /// One project.
    fn project(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Project>>> + Send;

    /// The projects of an organization, in identifier order after the page's cursor.
    fn projects(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Project>>> + Send;

    /// The workloads organization `organization`'s suspension stopped, each with the power
    /// version it declared (§2.1 of docs/08-identity.md).
    fn suspension_stops(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<(Uuid, u64)>>> + Send;
}

/// Writing organizations and projects.
pub trait TenancyWrite {
    /// Whether any organization exists: an installation has happened.
    fn any_organization(&mut self) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Inserts an organization. A sibling with the same name is
    /// [`Unique::OrganizationName`](super::Unique::OrganizationName).
    fn insert_organization(
        &mut self,
        organization: &Organization,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Sets organization `id`'s status to `to` if it is suspended now exactly when `suspended`
    /// says so, bumping its version; the organization as it now is, or `None` if it does not
    /// exist or its status was not the one expected.
    fn change_organization_status(
        &mut self,
        id: Uuid,
        suspended: bool,
        to: &OrganizationStatus,
    ) -> impl Future<Output = StoreResult<Option<Organization>>> + Send;

    /// Records that organization `organization`'s suspension stopped `workload` at power version
    /// `power_version`.
    fn record_suspension_stop(
        &mut self,
        organization: Uuid,
        workload: Uuid,
        power_version: u64,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Forgets what organization `organization`'s suspension stopped, as resuming it does.
    fn clear_suspension_stops(
        &mut self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Inserts a project. A project with the same name in the organization is
    /// [`Unique::ProjectName`](super::Unique::ProjectName).
    fn insert_project(&mut self, project: &Project)
    -> impl Future<Output = StoreResult<()>> + Send;
}
