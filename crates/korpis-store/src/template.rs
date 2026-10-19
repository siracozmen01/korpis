// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grant templates (migration 0091): the head names the current version and whether the
//! template is retired, and every version is kept whole, its body one document.

use crate::errors::from_sqlx;
use crate::stored::{
    DelegationDoc, SelectorDoc, actions, count, from_json, from_time, principal, to_count, to_json,
    to_time,
};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::page::Page;
use korpis_app::ports::{StoreError, StoreResult, TemplateRead, TemplateWrite};
use korpis_core::authz::Kind;
use korpis_core::authz::template::{
    GrantTemplate, ScopePattern, TemplateBody, TemplateConditions, TemplateVersion,
};
use korpis_core::tenancy::Name;
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A version's body.
#[derive(Debug, Serialize, Deserialize)]
struct BodyDoc {
    actions: Vec<String>,
    kind: Option<String>,
    selector: Option<SelectorDoc>,
    lifetime_ms: Option<u64>,
    source_cidr: Vec<String>,
    requires_mfa: bool,
    max_uses: Option<u64>,
    delegation: DelegationDoc,
}

impl BodyDoc {
    fn from_domain(b: &TemplateBody) -> StoreResult<Self> {
        let lifetime_ms =
            b.conditions
                .lifetime
                .map(|l| u64::try_from(l.as_millis()))
                .transpose()
                .map_err(|_| StoreError::Corrupt("template lifetime is out of range".into()))?;
        Ok(Self {
            actions: b.actions.iter().map(|a| a.as_str().to_owned()).collect(),
            kind: b.pattern.kind.map(|k| k.as_str().to_owned()),
            selector: b.pattern.selector.as_ref().map(SelectorDoc::from_domain),
            lifetime_ms,
            source_cidr: b.conditions.source_cidr.iter().map(ToString::to_string).collect(),
            requires_mfa: b.conditions.requires_mfa,
            max_uses: b.conditions.max_uses,
            delegation: DelegationDoc::from_domain(&b.delegation),
        })
    }

    fn into_domain(self) -> StoreResult<TemplateBody> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("template {what}"));
        let kind = self.kind.map(|k| Kind::parse(&k).ok_or_else(|| corrupt("kind"))).transpose()?;
        let source_cidr = self
            .source_cidr
            .iter()
            .map(|n| n.parse().map_err(|_| corrupt("source_cidr")))
            .collect::<StoreResult<_>>()?;
        Ok(TemplateBody {
            actions: actions(&self.actions)?,
            pattern: ScopePattern { kind, selector: self.selector.map(SelectorDoc::into_domain) },
            conditions: TemplateConditions {
                lifetime: self.lifetime_ms.map(Duration::from_millis),
                source_cidr,
                requires_mfa: self.requires_mfa,
                max_uses: self.max_uses,
            },
            delegation: self.delegation.into_domain()?,
        })
    }
}

struct VersionRow {
    version: i64,
    name: String,
    body: serde_json::Value,
    made_by: String,
    made_at: DateTime<Utc>,
}

impl VersionRow {
    fn into_domain(self) -> StoreResult<TemplateVersion> {
        let body: BodyDoc = from_json("template body", self.body)?;
        Ok(TemplateVersion {
            version: count("template version", self.version)?,
            name: Name::parse(self.name).map_err(|e| StoreError::Corrupt(e.to_string()))?,
            body: body.into_domain()?,
            by: principal(&self.made_by)?,
            at: to_time(self.made_at),
        })
    }
}

struct TemplateRow {
    id: Uuid,
    organization_id: Uuid,
    retired_at: Option<DateTime<Utc>>,
    version: i64,
    name: String,
    body: serde_json::Value,
    made_by: String,
    made_at: DateTime<Utc>,
}

impl TemplateRow {
    fn into_domain(self) -> StoreResult<GrantTemplate> {
        let current = VersionRow {
            version: self.version,
            name: self.name,
            body: self.body,
            made_by: self.made_by,
            made_at: self.made_at,
        };
        Ok(GrantTemplate {
            id: self.id,
            organization: self.organization_id,
            current: current.into_domain()?,
            retired_at: self.retired_at.map(to_time),
        })
    }
}

impl TemplateRead for PgStore {
    async fn template(&self, id: Uuid) -> StoreResult<Option<GrantTemplate>> {
        let row = sqlx::query_as!(
            TemplateRow,
            "SELECT t.id, t.organization_id, t.retired_at, v.version, v.name, v.body, v.made_by,
                    v.made_at
               FROM grant_template t
               JOIN grant_template_version v ON v.template_id = t.id AND v.version = t.version
              WHERE t.id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(TemplateRow::into_domain).transpose()
    }

    async fn template_version(
        &self,
        id: Uuid,
        version: u64,
    ) -> StoreResult<Option<TemplateVersion>> {
        let row = sqlx::query_as!(
            VersionRow,
            "SELECT version, name, body, made_by, made_at FROM grant_template_version
              WHERE template_id = $1 AND version = $2",
            id,
            to_count("template version", version)?
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(VersionRow::into_domain).transpose()
    }

    async fn templates_of(
        &self,
        owners: &[Uuid],
        live: bool,
        page: Page,
    ) -> StoreResult<Vec<GrantTemplate>> {
        let rows = sqlx::query_as!(
            TemplateRow,
            "SELECT t.id, t.organization_id, t.retired_at, v.version, v.name, v.body, v.made_by,
                    v.made_at
               FROM grant_template t
               JOIN grant_template_version v ON v.template_id = t.id AND v.version = t.version
              WHERE t.organization_id = ANY($1) AND (NOT $2 OR t.retired_at IS NULL)
                AND ($3::uuid IS NULL OR t.id > $3)
              ORDER BY t.id LIMIT $4",
            owners,
            live,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(TemplateRow::into_domain).collect()
    }
}

/// Inserts one version of template `id`.
async fn insert_version(tx: &mut PgTx, id: Uuid, v: &TemplateVersion) -> StoreResult<()> {
    sqlx::query!(
        "INSERT INTO grant_template_version (template_id, version, name, body, made_by, made_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        to_count("template version", v.version)?,
        v.name.as_str(),
        to_json("template body", &BodyDoc::from_domain(&v.body)?)?,
        v.by.key(),
        from_time(v.at)
    )
    .execute(&mut *tx.tx)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

impl TemplateWrite for PgTx {
    async fn insert_template(&mut self, t: &GrantTemplate) -> StoreResult<()> {
        sqlx::query!(
            "INSERT INTO grant_template (id, organization_id, name, version, retired_at)
             VALUES ($1, $2, $3, $4, $5)",
            t.id,
            t.organization,
            t.current.name.as_str(),
            to_count("template version", t.current.version)?,
            t.retired_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        insert_version(self, t.id, &t.current).await
    }

    async fn update_template(
        &mut self,
        id: Uuid,
        expected: u64,
        version: &TemplateVersion,
    ) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE grant_template SET version = $3, name = $4
              WHERE id = $1 AND version = $2 AND retired_at IS NULL",
            id,
            to_count("template version", expected)?,
            to_count("template version", version.version)?,
            version.name.as_str()
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        if moved == 0 {
            return Ok(false);
        }
        insert_version(self, id, version).await?;
        Ok(true)
    }

    async fn retire_template(&mut self, id: Uuid, at: SystemTime) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE grant_template SET retired_at = $2 WHERE id = $1 AND retired_at IS NULL",
            id,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(moved == 1)
    }
}
