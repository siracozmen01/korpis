// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portals (migration 0110): names the control plane answers under for an organization. A name is
//! one portal's at most, which the key holds; what else may not have it is checked by the
//! application under the domains lock.

use crate::errors::from_sqlx;
use crate::stored::{from_time, to_time};
use crate::{PgStore, PgTx};
use korpis_app::page::Page;
use korpis_app::ports::{PortalRead, PortalWrite, StoreError, StoreResult};
use korpis_core::dns::domain::DomainName;
use korpis_core::dns::portal::Portal;
use std::time::SystemTime;
use uuid::Uuid;

struct PortalRow {
    id: Uuid,
    organization_id: Uuid,
    name: String,
    certificate_id: Uuid,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<PortalRow> for Portal {
    type Error = StoreError;

    fn try_from(r: PortalRow) -> StoreResult<Self> {
        Ok(Self {
            id: r.id,
            organization: r.organization_id,
            name: DomainName::parse(&r.name)
                .ok_or_else(|| StoreError::Corrupt(format!("portal {}: name", r.id)))?,
            certificate: r.certificate_id,
            created_at: to_time(r.created_at),
        })
    }
}

fn portals(rows: Vec<PortalRow>) -> StoreResult<Vec<Portal>> {
    rows.into_iter().map(Portal::try_from).collect()
}

fn texts(names: &[DomainName]) -> Vec<String> {
    names.iter().map(|n| n.as_str().to_owned()).collect()
}

impl PortalRead for PgStore {
    async fn portal(&self, id: Uuid) -> StoreResult<Option<Portal>> {
        let row = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal
              WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(Portal::try_from).transpose()
    }

    async fn portals_of(&self, organization: Uuid, page: Page) -> StoreResult<Vec<Portal>> {
        let rows = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal
              WHERE organization_id = $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            organization,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        portals(rows)
    }

    async fn all_portals(&self) -> StoreResult<Vec<Portal>> {
        let rows = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal ORDER BY name"
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        portals(rows)
    }

    async fn portal_challenge(&self, name: &str) -> StoreResult<Option<Vec<u8>>> {
        sqlx::query_scalar!("SELECT digest FROM portal_challenge WHERE name = $1", name)
            .fetch_optional(&self.pool)
            .await
            .map_err(from_sqlx)
    }

    async fn portals_named(&self, names: &[DomainName]) -> StoreResult<Vec<Portal>> {
        let rows = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal
              WHERE name = ANY($1) ORDER BY name",
            &texts(names)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        portals(rows)
    }
}

impl PortalWrite for PgTx {
    async fn portals_beneath(&mut self, name: &DomainName) -> StoreResult<Vec<Portal>> {
        let rows = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal
              WHERE name = $1 OR right(name, length($1) + 1) = '.' || $1
              ORDER BY name",
            name.as_str()
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        portals(rows)
    }

    async fn portals_named_now(&mut self, names: &[DomainName]) -> StoreResult<Vec<Portal>> {
        let rows = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal
              WHERE name = ANY($1) ORDER BY name",
            &texts(names)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        portals(rows)
    }

    async fn acme_portal_certificate(
        &mut self,
        organization: Uuid,
        portal: Uuid,
        name: &DomainName,
        at: SystemTime,
    ) -> StoreResult<Uuid> {
        let id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO certificate (id, organization_id, names, source, chain, state, revision,
                                      created_at, portal_id)
             VALUES ($1, $2, $3, 'acme', '', 'pending', 0, $4, $5)",
            id,
            organization,
            &[name.as_str().to_owned()],
            from_time(at),
            portal
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(id)
    }

    async fn insert_portal(&mut self, p: &Portal) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO portal (id, organization_id, name, certificate_id, created_at)
             VALUES ($1, $2, $3, $4, $5)",
            p.id,
            p.organization,
            p.name.as_str(),
            p.certificate,
            from_time(p.created_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn lock_portal(&mut self, id: Uuid) -> StoreResult<Option<Portal>> {
        let row = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal
              WHERE id = $1 FOR UPDATE",
            id
        )
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        row.map(Portal::try_from).transpose()
    }

    async fn set_portal_certificate(&mut self, id: Uuid, certificate: Uuid) -> StoreResult<()> {
        sqlx::query!("UPDATE portal SET certificate_id = $2 WHERE id = $1", id, certificate)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn portals_served_with(&mut self, certificate: Uuid) -> StoreResult<Vec<Portal>> {
        let rows = sqlx::query_as!(
            PortalRow,
            "SELECT id, organization_id, name, certificate_id, created_at FROM portal
              WHERE certificate_id = $1 ORDER BY name",
            certificate
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        portals(rows)
    }

    async fn put_portal_challenge(
        &mut self,
        name: &str,
        certificate: Uuid,
        digest: &[u8],
    ) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO portal_challenge (name, certificate_id, digest) VALUES ($1, $2, $3)
             ON CONFLICT (name) DO UPDATE SET certificate_id = $2, digest = $3",
            name,
            certificate,
            digest
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn clear_portal_challenges(&mut self, certificate: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM portal_challenge WHERE certificate_id = $1", certificate)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn delete_portal(&mut self, id: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM portal WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }
}
