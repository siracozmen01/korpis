// SPDX-License-Identifier: AGPL-3.0-or-later

//! Extension installations (§4 of docs/16-extensions.md).

use crate::errors::from_sqlx;
use crate::stored::{from_time, principal, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::Page;
use korpis_app::ports::{ExtensionRead, ExtensionWrite, StoreResult};
use korpis_core::extension::{Installation, ProviderType};
use std::time::SystemTime;
use uuid::Uuid;

struct ExtensionRow {
    id: Uuid,
    organization_id: Uuid,
    project_id: Option<Uuid>,
    name: String,
    version: String,
    api_version: String,
    workload_id: Uuid,
    artifact_name: String,
    artifact_digest: String,
    installed_by: String,
    installed_at: DateTime<Utc>,
    uninstalled_at: Option<DateTime<Utc>>,
}

impl ExtensionRow {
    fn into_domain(self) -> StoreResult<Installation> {
        Ok(Installation {
            id: self.id,
            organization: self.organization_id,
            project: self.project_id,
            name: self.name,
            version: self.version,
            api_version: self.api_version,
            workload: self.workload_id,
            artifact_name: self.artifact_name,
            artifact_digest: self.artifact_digest,
            installed_by: principal(&self.installed_by)?,
            installed_at: to_time(self.installed_at),
            uninstalled_at: self.uninstalled_at.map(to_time),
        })
    }
}

impl ExtensionRead for PgStore {
    async fn extension(&self, id: Uuid) -> StoreResult<Option<Installation>> {
        let row = sqlx::query_as!(
            ExtensionRow,
            "SELECT id, organization_id, project_id, name, version, api_version, workload_id,
                    artifact_name, artifact_digest, installed_by, installed_at, uninstalled_at
               FROM extension WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(ExtensionRow::into_domain).transpose()
    }

    async fn extension_of_workload(&self, workload: Uuid) -> StoreResult<Option<Installation>> {
        let row = sqlx::query_as!(
            ExtensionRow,
            "SELECT id, organization_id, project_id, name, version, api_version, workload_id,
                    artifact_name, artifact_digest, installed_by, installed_at, uninstalled_at
               FROM extension WHERE workload_id = $1",
            workload
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(ExtensionRow::into_domain).transpose()
    }

    async fn installed_extension(
        &self,
        organization: Uuid,
        project: Option<Uuid>,
        name: &str,
    ) -> StoreResult<Option<Installation>> {
        let row = sqlx::query_as!(
            ExtensionRow,
            "SELECT id, organization_id, project_id, name, version, api_version, workload_id,
                    artifact_name, artifact_digest, installed_by, installed_at, uninstalled_at
               FROM extension
              WHERE organization_id = $1 AND project_id IS NOT DISTINCT FROM $2 AND name = $3
                AND uninstalled_at IS NULL",
            organization,
            project,
            name
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(ExtensionRow::into_domain).transpose()
    }

    async fn providing_installation(
        &self,
        organization: Uuid,
        kind: ProviderType,
        name: &str,
    ) -> StoreResult<Option<Installation>> {
        let row = sqlx::query_as!(
            ExtensionRow,
            "SELECT e.id, e.organization_id, e.project_id, e.name, e.version, e.api_version,
                    e.workload_id, e.artifact_name, e.artifact_digest, e.installed_by,
                    e.installed_at, e.uninstalled_at
               FROM extension e JOIN extension_provides p ON p.extension_id = e.id
              WHERE p.organization_id = $1 AND p.type = $2 AND p.name = $3
                AND e.uninstalled_at IS NULL",
            organization,
            kind.as_str(),
            name
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(ExtensionRow::into_domain).transpose()
    }

    async fn extensions(&self, organization: Uuid, page: Page) -> StoreResult<Vec<Installation>> {
        let rows = sqlx::query_as!(
            ExtensionRow,
            "SELECT id, organization_id, project_id, name, version, api_version, workload_id,
                    artifact_name, artifact_digest, installed_by, installed_at, uninstalled_at
               FROM extension
              WHERE organization_id = $1 AND uninstalled_at IS NULL
                AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            organization,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(ExtensionRow::into_domain).collect()
    }
}

impl ExtensionWrite for PgTx {
    async fn insert_extension(&mut self, e: &Installation) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO extension (id, organization_id, project_id, name, version, api_version,
                                    workload_id, artifact_name, artifact_digest, installed_by,
                                    installed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            e.id,
            e.organization,
            e.project,
            e.name,
            e.version,
            e.api_version,
            e.workload,
            e.artifact_name,
            e.artifact_digest,
            e.installed_by.key(),
            from_time(e.installed_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn upgrade_extension(
        &mut self,
        from_digest: &str,
        to: &Installation,
    ) -> StoreResult<bool> {
        let n = sqlx::query!(
            "UPDATE extension SET version = $3, api_version = $4, artifact_digest = $5
              WHERE id = $1 AND artifact_digest = $2 AND uninstalled_at IS NULL",
            to.id,
            from_digest,
            to.version,
            to.api_version,
            to.artifact_digest
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(n == 1)
    }

    async fn uninstall_extension(&mut self, id: Uuid, at: SystemTime) -> StoreResult<bool> {
        let n = sqlx::query!(
            "UPDATE extension SET uninstalled_at = $2 WHERE id = $1 AND uninstalled_at IS NULL",
            id,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(n == 1)
    }
}
