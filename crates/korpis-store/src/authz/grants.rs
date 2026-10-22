// SPDX-License-Identifier: AGPL-3.0-or-later

//! Grants. Revocation is written on one row and derived for the subtree (§3.5 of
//! docs/08-identity.md), so nothing here ever walks downwards to revoke.

use crate::errors::from_sqlx;
use crate::stored::{
    ConditionsDoc, DelegationDoc, ScopeDoc, actions, count, from_json, from_time, principal,
    to_count, to_json, to_time,
};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::Page;
use korpis_app::ports::{GrantRead, GrantWrite, StoreResult};
use korpis_core::authz::{Grant, GrantRecord, ObjectRef, TemplateRef, Via};
use serde::{Deserialize, Serialize};
use sqlx::PgExecutor;
use std::time::SystemTime;
use uuid::Uuid;

struct GrantRow {
    id: Uuid,
    subject_key: String,
    actions: Vec<String>,
    scope: serde_json::Value,
    conditions: serde_json::Value,
    parent_id: Option<Uuid>,
    delegation: serde_json::Value,
    issued_by: String,
    link_form: bool,
    uses: i64,
    issued_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    suspended_at: Option<DateTime<Utc>>,
    root: bool,
    irreversible_override: bool,
    label: String,
    template_id: Option<Uuid>,
    template_version: Option<i64>,
    via: Option<serde_json::Value>,
}

/// A grant's `via`, as kept: the set's and the member's subject keys.
#[derive(Debug, Serialize, Deserialize)]
struct ViaDoc {
    set: String,
    member: String,
}

fn via_from(v: Option<serde_json::Value>) -> StoreResult<Option<Via>> {
    let Some(v) = v else { return Ok(None) };
    let doc: ViaDoc = from_json("via", v)?;
    Ok(Some(Via { set: principal(&doc.set)?.subject(), member: principal(&doc.member)?.subject() }))
}

impl GrantRow {
    fn into_domain(self) -> StoreResult<GrantRecord> {
        let holder = principal(&self.subject_key)?;
        let scope: ScopeDoc = from_json("scope", self.scope)?;
        let conditions: ConditionsDoc = from_json("conditions", self.conditions)?;
        let delegation: DelegationDoc = from_json("delegation", self.delegation)?;
        Ok(GrantRecord {
            grant: Grant {
                id: self.id,
                issued_at: to_time(self.issued_at),
                via: via_from(self.via)?,
                subject: holder.subject(),
                actions: actions(&self.actions)?,
                scope: scope.into_domain()?,
                conditions: conditions.into_domain()?,
                parent: self.parent_id,
                delegation: delegation.into_domain()?,
                link_form: self.link_form,
                uses: count("uses", self.uses)?,
                revoked: self.revoked_at.is_some(),
                suspended: self.suspended_at.is_some(),
                irreversible_override: self.irreversible_override,
                root: self.root,
            },
            holder,
            issued_by: principal(&self.issued_by)?,
            issued_at: to_time(self.issued_at),
            revoked_at: self.revoked_at.map(to_time),
            suspended_at: self.suspended_at.map(to_time),
            label: self.label,
            from_template: match (self.template_id, self.template_version) {
                (Some(template), Some(version)) => {
                    Some(TemplateRef { template, version: count("template_version", version)? })
                }
                _ => None,
            },
        })
    }
}

fn all(rows: Vec<GrantRow>) -> StoreResult<Vec<GrantRecord>> {
    rows.into_iter().map(GrantRow::into_domain).collect()
}

/// The chain of `id`, leaf first and root last, in one statement.
async fn chain(db: impl PgExecutor<'_>, id: Uuid) -> StoreResult<Vec<GrantRecord>> {
    let rows = sqlx::query_as!(
        GrantRow,
        r#"WITH RECURSIVE c AS (
               SELECT g.*, 0 AS depth FROM grants g WHERE g.id = $1
               UNION ALL
               SELECT g.*, c.depth + 1 FROM grants g JOIN c ON g.id = c.parent_id
           )
           SELECT id AS "id!", subject_key AS "subject_key!", actions AS "actions!",
                  scope AS "scope!", conditions AS "conditions!", parent_id,
                  delegation AS "delegation!", issued_by AS "issued_by!",
                  link_form AS "link_form!", uses AS "uses!", issued_at AS "issued_at!",
                  revoked_at, suspended_at, root AS "root!",
                  irreversible_override AS "irreversible_override!", label AS "label!",
                  template_id, template_version, via
           FROM c ORDER BY depth"#,
        id
    )
    .fetch_all(db)
    .await
    .map_err(from_sqlx)?;
    all(rows)
}

/// Records the member a grant names in its `via`, unverified, and makes their set's binding due,
/// so its next question asks about them (§5.1 of docs/08-identity.md): every way of issuing
/// such a grant inserts it here, so none forgets.
async fn record_member(db: impl PgExecutor<'_>, v: &Via, at: SystemTime) -> StoreResult<()> {
    sqlx::query!(
        "WITH b AS (SELECT id FROM identity_binding WHERE subject = $1 AND retired_at IS NULL),
              recorded AS (
                  INSERT INTO identity_member (binding_id, member, state, recorded_at)
                  SELECT b.id, $2, 'unverified', $3 FROM b ON CONFLICT DO NOTHING)
         UPDATE identity_binding SET verify_due_at = LEAST(verify_due_at, $3)
          WHERE id IN (SELECT id FROM b)",
        v.set.key,
        v.member.key,
        from_time(at)
    )
    .execute(db)
    .await
    .map_err(from_sqlx)?;
    Ok(())
}

impl GrantRead for PgStore {
    async fn grant_chain(&self, grant: Uuid) -> StoreResult<Vec<GrantRecord>> {
        chain(&self.pool, grant).await
    }

    async fn chains_held_by(&self, subject_key: &str) -> StoreResult<Vec<Vec<GrantRecord>>> {
        let leaves = sqlx::query_scalar!(
            "SELECT id FROM grants WHERE subject_key = $1 AND revoked_at IS NULL ORDER BY issued_at",
            subject_key
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        let mut chains = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            chains.push(chain(&self.pool, leaf).await?);
        }
        Ok(chains)
    }

    async fn grants_on(&self, object: ObjectRef, page: Page) -> StoreResult<Vec<GrantRecord>> {
        let rows = sqlx::query_as!(
            GrantRow,
            "SELECT id, subject_key, actions, scope, conditions, parent_id, delegation, issued_by,
                    link_form, uses, issued_at, revoked_at, suspended_at, root,
                    irreversible_override, label, template_id, template_version, via
               FROM grants
              WHERE scope_kind = $1 AND scope_id = $2 AND ($3::uuid IS NULL OR id > $3)
              ORDER BY id LIMIT $4",
            object.kind.as_str(),
            object.id,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        all(rows)
    }

    async fn grants_from_template(
        &self,
        template: Uuid,
        page: Page,
    ) -> StoreResult<Vec<GrantRecord>> {
        let rows = sqlx::query_as!(
            GrantRow,
            "SELECT id, subject_key, actions, scope, conditions, parent_id, delegation, issued_by,
                    link_form, uses, issued_at, revoked_at, suspended_at, root,
                    irreversible_override, label, template_id, template_version, via
               FROM grants
              WHERE template_id = $1 AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            template,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        all(rows)
    }

    async fn grants_held(
        &self,
        subject_keys: &[String],
        page: Page,
    ) -> StoreResult<Vec<GrantRecord>> {
        let rows = sqlx::query_as!(
            GrantRow,
            "SELECT id, subject_key, actions, scope, conditions, parent_id, delegation, issued_by,
                    link_form, uses, issued_at, revoked_at, suspended_at, root,
                    irreversible_override, label, template_id, template_version, via
               FROM grants
              WHERE subject_key = ANY($1) AND ($2::uuid IS NULL OR id > $2)
              ORDER BY id LIMIT $3",
            subject_keys,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        all(rows)
    }

    async fn revoked_since(&self, since: SystemTime) -> StoreResult<Vec<Uuid>> {
        sqlx::query_scalar!(
            "SELECT id AS \"id!\" FROM grants WHERE revoked_at >= $1
             UNION
             SELECT id FROM grants WHERE revoked_at IS NULL AND suspended_at >= $1
             ORDER BY 1",
            from_time(since)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)
    }
}

impl GrantWrite for PgTx {
    async fn insert_grant(&mut self, r: &GrantRecord) -> StoreResult<()> {
        let g = &r.grant;
        let anchor = g.scope.anchor();
        let actions: Vec<String> = g.actions.iter().map(|a| a.as_str().to_owned()).collect();
        sqlx::query!(
            "INSERT INTO grants (id, subject_key, actions, scope, scope_kind, scope_id, conditions,
                                 parent_id, delegation, issued_by, link_form, uses, issued_at,
                                 root, irreversible_override, label, template_id,
                                 template_version, via)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                     $17, $18, $19)",
            g.id,
            g.subject.key,
            &actions,
            to_json("scope", &ScopeDoc::from_domain(&g.scope))?,
            anchor.kind.as_str(),
            anchor.id,
            to_json("conditions", &ConditionsDoc::from_domain(&g.conditions))?,
            g.parent,
            to_json("delegation", &DelegationDoc::from_domain(&g.delegation))?,
            r.issued_by.key(),
            g.link_form,
            to_count("uses", g.uses)?,
            from_time(r.issued_at),
            g.root,
            g.irreversible_override,
            r.label,
            r.from_template.map(|t| t.template),
            r.from_template.map(|t| to_count("template_version", t.version)).transpose()?,
            g.via
                .as_ref()
                .map(|v| {
                    let doc = ViaDoc { set: v.set.key.clone(), member: v.member.key.clone() };
                    to_json("via", &doc)
                })
                .transpose()?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        if let Some(v) = &g.via {
            record_member(&mut *self.tx, v, r.issued_at).await?;
        }
        Ok(())
    }

    async fn revoke_grant(&mut self, grant: Uuid, at: SystemTime) -> StoreResult<bool> {
        let n = sqlx::query!(
            "UPDATE grants SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL",
            grant,
            from_time(at)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(n == 1)
    }

    async fn grants_held_now(&mut self, subject_key: &str) -> StoreResult<Vec<GrantRecord>> {
        let rows = sqlx::query_as!(
            GrantRow,
            "SELECT id, subject_key, actions, scope, conditions, parent_id, delegation, issued_by,
                    link_form, uses, issued_at, revoked_at, suspended_at, root,
                    irreversible_override, label, template_id, template_version, via
               FROM grants
              WHERE subject_key = $1 AND revoked_at IS NULL
              ORDER BY id",
            subject_key
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        all(rows)
    }

    async fn descendant_count(&mut self, grant: Uuid) -> StoreResult<u64> {
        let n = sqlx::query_scalar!(
            r#"WITH RECURSIVE d AS (
                   SELECT id FROM grants WHERE parent_id = $1
                   UNION ALL
                   SELECT g.id FROM grants g JOIN d ON g.parent_id = d.id
               )
               SELECT count(*) AS "n!" FROM d"#,
            grant
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        count("descendants", n)
    }

    async fn take_uses(&mut self, grant: Uuid, n: u64, max: u64) -> StoreResult<bool> {
        let taken = sqlx::query!(
            "UPDATE grants SET uses = uses + $2 WHERE id = $1 AND uses + $2 <= $3",
            grant,
            to_count("uses", n)?,
            to_count("max_uses", max)?
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(taken == 1)
    }
}
