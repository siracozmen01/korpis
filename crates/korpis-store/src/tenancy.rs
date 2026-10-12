// SPDX-License-Identifier: AGPL-3.0-or-later

//! Organizations, projects, and the object hierarchy.

mod hierarchy;

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, to_count, to_json, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::Page;
use korpis_app::ports::{StoreError, StoreResult, TenancyRead, TenancyWrite};
use korpis_core::labels::Labels;
use korpis_core::tenancy::{Name, Organization, OrganizationStatus, Project};
use std::collections::BTreeMap;
use uuid::Uuid;

struct OrganizationRow {
    id: Uuid,
    name: String,
    parent_id: Option<Uuid>,
    status: String,
    version: i64,
    created: DateTime<Utc>,
}

impl OrganizationRow {
    fn into_domain(self) -> StoreResult<Organization> {
        let status = match self.status.as_str() {
            "active" => OrganizationStatus::Active,
            "suspended" => OrganizationStatus::Suspended,
            other => return Err(StoreError::Corrupt(format!("organization status {other:?}"))),
        };
        Ok(Organization {
            id: self.id,
            name: name(self.name)?,
            parent: self.parent_id,
            status,
            version: count("organization version", self.version)?,
            created: to_time(self.created),
        })
    }
}

struct ProjectRow {
    id: Uuid,
    organization_id: Uuid,
    name: String,
    labels: serde_json::Value,
    version: i64,
    created: DateTime<Utc>,
}

impl ProjectRow {
    fn into_domain(self) -> StoreResult<Project> {
        Ok(Project {
            id: self.id,
            organization: self.organization_id,
            name: name(self.name)?,
            labels: labels(self.labels)?,
            version: count("project version", self.version)?,
            created: to_time(self.created),
        })
    }
}

fn name(s: String) -> StoreResult<Name> {
    Name::parse(s).map_err(|e| StoreError::Corrupt(format!("name: {e}")))
}

/// Stored labels.
pub(crate) fn labels(v: serde_json::Value) -> StoreResult<Labels> {
    let map: BTreeMap<String, String> = from_json("labels", v)?;
    Labels::new(map).map_err(|e| StoreError::Corrupt(e.to_string()))
}

fn status(s: OrganizationStatus) -> &'static str {
    match s {
        OrganizationStatus::Active => "active",
        OrganizationStatus::Suspended => "suspended",
    }
}

impl TenancyRead for PgStore {
    async fn organization(&self, id: Uuid) -> StoreResult<Option<Organization>> {
        sqlx::query_as!(
            OrganizationRow,
            "SELECT id, name, parent_id, status, version, created FROM organization WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?
        .map(OrganizationRow::into_domain)
        .transpose()
    }

    async fn organizations(
        &self,
        parent: Option<Uuid>,
        page: Page,
    ) -> StoreResult<Vec<Organization>> {
        sqlx::query_as!(
            OrganizationRow,
            "SELECT id, name, parent_id, status, version, created FROM organization
              WHERE parent_id IS NOT DISTINCT FROM $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            parent,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?
        .into_iter()
        .map(OrganizationRow::into_domain)
        .collect()
    }

    async fn project(&self, id: Uuid) -> StoreResult<Option<Project>> {
        sqlx::query_as!(
            ProjectRow,
            "SELECT id, organization_id, name, labels, version, created FROM project WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?
        .map(ProjectRow::into_domain)
        .transpose()
    }

    async fn projects(&self, organization: Uuid, page: Page) -> StoreResult<Vec<Project>> {
        sqlx::query_as!(
            ProjectRow,
            "SELECT id, organization_id, name, labels, version, created FROM project
              WHERE organization_id = $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            organization,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?
        .into_iter()
        .map(ProjectRow::into_domain)
        .collect()
    }
}

impl TenancyWrite for PgTx {
    async fn any_organization(&mut self) -> StoreResult<bool> {
        sqlx::query_scalar!("SELECT EXISTS (SELECT 1 FROM organization) AS \"e!\"")
            .fetch_one(&mut *self.tx)
            .await
            .map_err(from_sqlx)
    }

    async fn insert_organization(&mut self, o: &Organization) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO organization (id, name, parent_id, status, version, created)
             VALUES ($1, $2, $3, $4, $5, $6)",
            o.id,
            o.name.as_str(),
            o.parent,
            status(o.status),
            to_count("version", o.version)?,
            from_time(o.created)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn insert_project(&mut self, p: &Project) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO project (id, organization_id, name, labels, version, created)
             VALUES ($1, $2, $3, $4, $5, $6)",
            p.id,
            p.organization,
            p.name.as_str(),
            to_json("labels", p.labels.as_map())?,
            to_count("version", p.version)?,
            from_time(p.created)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }
}
