// SPDX-License-Identifier: AGPL-3.0-or-later

//! Providers (§3.3 of docs/16-extensions.md, migration 0026).

use crate::errors::from_sqlx;
use crate::stored::{count, from_time, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::ports::{ProviderRead, ProviderWrite, StoreError, StoreResult};
use korpis_core::extension::{ProvidedInterface, ProviderType, Registration};
use std::time::SystemTime;
use uuid::Uuid;

pub(crate) fn kind(s: &str) -> StoreResult<ProviderType> {
    ProviderType::parse(s).ok_or_else(|| StoreError::Corrupt(format!("provider type {s:?}")))
}

pub(crate) struct RegistrationRow {
    pub(crate) id: Uuid,
    pub(crate) extension_id: Uuid,
    pub(crate) organization_id: Uuid,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) concurrency: i32,
    pub(crate) sign_in: bool,
    pub(crate) replica: Uuid,
    pub(crate) connected_at: DateTime<Utc>,
    pub(crate) seen_at: DateTime<Utc>,
}

impl RegistrationRow {
    pub(crate) fn into_domain(self) -> StoreResult<Registration> {
        Ok(Registration {
            id: self.id,
            extension: self.extension_id,
            organization: self.organization_id,
            kind: kind(&self.kind)?,
            name: self.name,
            version: self.version,
            concurrency: u32::try_from(count("concurrency", i64::from(self.concurrency))?)
                .map_err(|_| StoreError::Corrupt("concurrency".into()))?,
            sign_in: self.sign_in,
            replica: self.replica,
            connected_at: to_time(self.connected_at),
            seen_at: to_time(self.seen_at),
        })
    }
}

impl ProviderRead for PgStore {
    async fn provides_of(&self, extension: Uuid) -> StoreResult<Vec<ProvidedInterface>> {
        let rows = sqlx::query!(
            "SELECT type, name FROM extension_provides WHERE extension_id = $1
              ORDER BY type, name",
            extension
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter()
            .map(|r| Ok(ProvidedInterface { kind: kind(&r.r#type)?, name: r.name }))
            .collect()
    }

    async fn registrations(
        &self,
        organization: Uuid,
        k: ProviderType,
        name: &str,
    ) -> StoreResult<Vec<Registration>> {
        let rows = sqlx::query_as!(
            RegistrationRow,
            r#"SELECT id, extension_id, organization_id, type AS kind, name, version,
                      concurrency, sign_in, replica, connected_at, seen_at
                 FROM provider_registration
                WHERE organization_id = $1 AND type = $2 AND name = $3 ORDER BY id"#,
            organization,
            k.as_str(),
            name
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RegistrationRow::into_domain).collect()
    }
}

impl ProviderWrite for PgTx {
    async fn insert_provides(
        &mut self,
        extension: Uuid,
        organization: Uuid,
        provides: &[ProvidedInterface],
    ) -> StoreResult<()> {
        for p in provides {
            sqlx::query!(
                "INSERT INTO extension_provides (extension_id, organization_id, type, name)
                 VALUES ($1, $2, $3, $4)",
                extension,
                organization,
                p.kind.as_str(),
                p.name
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        }
        Ok(())
    }

    async fn replace_provides(
        &mut self,
        extension: Uuid,
        organization: Uuid,
        provides: &[ProvidedInterface],
    ) -> StoreResult<()> {
        let kinds: Vec<String> = provides.iter().map(|p| p.kind.as_str().to_owned()).collect();
        let names: Vec<String> = provides.iter().map(|p| p.name.clone()).collect();
        // What it keeps is the pairs of the two arrays, side by side.
        sqlx::query!(
            "DELETE FROM provider_registration r
              WHERE r.extension_id = $1
                AND (r.type, r.name) NOT IN (SELECT * FROM unnest($2::text[], $3::text[]))",
            extension,
            kinds.as_slice(),
            names.as_slice()
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        sqlx::query!(
            "DELETE FROM extension_provides p
              WHERE p.extension_id = $1
                AND (p.type, p.name) NOT IN (SELECT * FROM unnest($2::text[], $3::text[]))",
            extension,
            kinds.as_slice(),
            names.as_slice()
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        for p in provides {
            sqlx::query!(
                "INSERT INTO extension_provides (extension_id, organization_id, type, name)
                 VALUES ($1, $2, $3, $4) ON CONFLICT (extension_id, type, name) DO NOTHING",
                extension,
                organization,
                p.kind.as_str(),
                p.name
            )
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        }
        Ok(())
    }

    async fn delete_provides(&mut self, extension: Uuid) -> StoreResult<()> {
        sqlx::query!("DELETE FROM provider_registration WHERE extension_id = $1", extension)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        sqlx::query!("DELETE FROM extension_provides WHERE extension_id = $1", extension)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?;
        Ok(())
    }

    async fn insert_registration(&mut self, r: &Registration) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO provider_registration (id, extension_id, organization_id, type, name,
                                                version, concurrency, sign_in, replica,
                                                connected_at, seen_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            r.id,
            r.extension,
            r.organization,
            r.kind.as_str(),
            r.name,
            r.version,
            i32::try_from(r.concurrency).map_err(|_| StoreError::Corrupt("concurrency".into()))?,
            r.sign_in,
            r.replica,
            from_time(r.connected_at),
            from_time(r.seen_at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn delete_registration(&mut self, id: Uuid) -> StoreResult<bool> {
        let n = sqlx::query!("DELETE FROM provider_registration WHERE id = $1", id)
            .execute(&mut *self.tx)
            .await
            .map_err(from_sqlx)?
            .rows_affected();
        Ok(n == 1)
    }

    async fn touch_registrations(
        &mut self,
        replica: Uuid,
        at: SystemTime,
    ) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "UPDATE provider_registration SET seen_at = $2 WHERE replica = $1 RETURNING id",
            replica,
            from_time(at)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)
    }

    async fn expire_registrations(&mut self, before: SystemTime) -> StoreResult<Vec<Registration>> {
        let rows = sqlx::query_as!(
            RegistrationRow,
            r#"DELETE FROM provider_registration WHERE seen_at < $1
               RETURNING id, extension_id, organization_id, type AS kind, name, version,
                         concurrency, sign_in, replica, connected_at, seen_at"#,
            from_time(before)
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(RegistrationRow::into_domain).collect()
    }
}
