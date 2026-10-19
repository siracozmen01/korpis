// SPDX-License-Identifier: AGPL-3.0-or-later

//! Identity bindings (migration 0094): a row per binding, its required memberships one
//! document, its durations in milliseconds.

use crate::errors::from_sqlx;
use crate::stored::{count, from_json, from_time, principal, to_count, to_json, to_time};
use crate::{PgStore, PgTx};
use chrono::{DateTime, Utc};
use korpis_app::page::Page;
use korpis_app::ports::{BindingRead, BindingWrite, StoreError, StoreResult};
use korpis_core::authz::TemplateRef;
use korpis_core::authz::binding::{
    BindingOrigin, BindingState, BindingTerms, IdentityBinding, ProviderRef, SetName,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// One required membership.
#[derive(Debug, Serialize, Deserialize)]
struct SetDoc {
    realm: String,
    member_of: String,
}

fn millis(what: &str, d: Duration) -> StoreResult<i64> {
    i64::try_from(d.as_millis()).map_err(|_| StoreError::Corrupt(format!("{what} is too long")))
}

fn duration(what: &str, ms: i64) -> StoreResult<Duration> {
    u64::try_from(ms)
        .map(Duration::from_millis)
        .map_err(|_| StoreError::Corrupt(format!("{what} is negative")))
}

struct BindingRow {
    id: Uuid,
    organization_id: Uuid,
    provider: String,
    subject: String,
    origin: String,
    template_id: Option<Uuid>,
    template_version: Option<i64>,
    require_membership: serde_json::Value,
    grace_ms: i64,
    reverify_ms: i64,
    allows_irreversible: bool,
    state: String,
    last_verified_at: Option<DateTime<Utc>>,
    version: i64,
    made_by: String,
    made_at: DateTime<Utc>,
    retired_at: Option<DateTime<Utc>>,
}

impl BindingRow {
    fn into_domain(self) -> StoreResult<IdentityBinding> {
        let corrupt = |what: &str| StoreError::Corrupt(format!("binding {what}"));
        let template = match (self.template_id, self.template_version) {
            (Some(template), Some(v)) => {
                Some(TemplateRef { template, version: count("binding template version", v)? })
            }
            _ => None,
        };
        let sets: Vec<SetDoc> = from_json("binding memberships", self.require_membership)?;
        Ok(IdentityBinding {
            id: self.id,
            provider: ProviderRef { organization: self.organization_id, name: self.provider },
            subject: principal(&self.subject)?,
            origin: match self.origin.as_str() {
                "operator" => BindingOrigin::Operator,
                "first_use" => BindingOrigin::FirstUse,
                _ => return Err(corrupt("origin")),
            },
            terms: BindingTerms {
                template,
                require_membership: sets
                    .into_iter()
                    .map(|s| SetName { realm: s.realm, member_of: s.member_of })
                    .collect(),
                grace: duration("binding grace", self.grace_ms)?,
                reverify_interval: duration("binding interval", self.reverify_ms)?,
                allows_irreversible: self.allows_irreversible,
            },
            state: match self.state.as_str() {
                "verified" => BindingState::Verified,
                "unverified" => BindingState::Unverified,
                _ => return Err(corrupt("state")),
            },
            last_verified_at: self.last_verified_at.map(to_time),
            version: count("binding version", self.version)?,
            made_by: principal(&self.made_by)?,
            made_at: to_time(self.made_at),
            retired_at: self.retired_at.map(to_time),
        })
    }
}

const fn state_name(state: BindingState) -> &'static str {
    match state {
        BindingState::Verified => "verified",
        BindingState::Unverified => "unverified",
    }
}

fn memberships(terms: &BindingTerms) -> StoreResult<serde_json::Value> {
    let sets: Vec<SetDoc> = terms
        .require_membership
        .iter()
        .map(|s| SetDoc { realm: s.realm.clone(), member_of: s.member_of.clone() })
        .collect();
    to_json("binding memberships", &sets)
}

impl BindingRead for PgStore {
    async fn binding(&self, id: Uuid) -> StoreResult<Option<IdentityBinding>> {
        let row = sqlx::query_as!(
            BindingRow,
            "SELECT id, organization_id, provider, subject, origin, template_id, template_version,
                    require_membership, grace_ms, reverify_ms, allows_irreversible, state,
                    last_verified_at, version, made_by, made_at, retired_at
               FROM identity_binding WHERE id = $1",
            id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(BindingRow::into_domain).transpose()
    }

    async fn binding_of(&self, subject: &str) -> StoreResult<Option<IdentityBinding>> {
        let row = sqlx::query_as!(
            BindingRow,
            "SELECT id, organization_id, provider, subject, origin, template_id, template_version,
                    require_membership, grace_ms, reverify_ms, allows_irreversible, state,
                    last_verified_at, version, made_by, made_at, retired_at
               FROM identity_binding WHERE subject = $1 AND retired_at IS NULL",
            subject
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(from_sqlx)?;
        row.map(BindingRow::into_domain).transpose()
    }

    async fn bindings_in(
        &self,
        organization: Uuid,
        retired: bool,
        page: Page,
    ) -> StoreResult<Vec<IdentityBinding>> {
        let rows = sqlx::query_as!(
            BindingRow,
            "SELECT id, organization_id, provider, subject, origin, template_id, template_version,
                    require_membership, grace_ms, reverify_ms, allows_irreversible, state,
                    last_verified_at, version, made_by, made_at, retired_at
               FROM identity_binding
              WHERE organization_id = $1 AND ($2 OR retired_at IS NULL)
                AND ($3::uuid IS NULL OR id > $3)
              ORDER BY id LIMIT $4",
            organization,
            retired,
            page.after,
            i64::from(page.size)
        )
        .fetch_all(&self.pool)
        .await
        .map_err(from_sqlx)?;
        rows.into_iter().map(BindingRow::into_domain).collect()
    }
}

impl BindingWrite for PgTx {
    async fn insert_binding(&mut self, b: &IdentityBinding) -> StoreResult<()> {
        let t = &b.terms;
        sqlx::query!(
            "INSERT INTO identity_binding (id, organization_id, provider, subject, is_set, origin,
                    template_id, template_version, require_membership, grace_ms, reverify_ms,
                    allows_irreversible, state, last_verified_at, version, made_by, made_at,
                    retired_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                     $18)",
            b.id,
            b.provider.organization,
            b.provider.name,
            b.subject.key(),
            b.is_set(),
            match b.origin {
                BindingOrigin::Operator => "operator",
                BindingOrigin::FirstUse => "first_use",
            },
            t.template.map(|r| r.template),
            t.template.map(|r| to_count("binding template version", r.version)).transpose()?,
            memberships(t)?,
            millis("binding grace", t.grace)?,
            millis("binding interval", t.reverify_interval)?,
            t.allows_irreversible,
            state_name(b.state),
            b.last_verified_at.map(from_time),
            to_count("binding version", b.version)?,
            b.made_by.key(),
            from_time(b.made_at),
            b.retired_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?;
        Ok(())
    }

    async fn update_binding(
        &mut self,
        id: Uuid,
        expected: u64,
        t: &BindingTerms,
    ) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE identity_binding
                SET version = version + 1, template_id = $3, template_version = $4,
                    require_membership = $5, grace_ms = $6, reverify_ms = $7,
                    allows_irreversible = $8
              WHERE id = $1 AND version = $2 AND retired_at IS NULL",
            id,
            to_count("binding version", expected)?,
            t.template.map(|r| r.template),
            t.template.map(|r| to_count("binding template version", r.version)).transpose()?,
            memberships(t)?,
            millis("binding grace", t.grace)?,
            millis("binding interval", t.reverify_interval)?,
            t.allows_irreversible
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(moved == 1)
    }

    async fn record_verification(
        &mut self,
        id: Uuid,
        state: BindingState,
        last_verified_at: Option<SystemTime>,
    ) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE identity_binding SET state = $2, last_verified_at = $3
              WHERE id = $1 AND retired_at IS NULL",
            id,
            state_name(state),
            last_verified_at.map(from_time)
        )
        .execute(&mut *self.tx)
        .await
        .map_err(from_sqlx)?
        .rows_affected();
        Ok(moved == 1)
    }

    async fn retire_binding(&mut self, id: Uuid, at: SystemTime) -> StoreResult<bool> {
        let moved = sqlx::query!(
            "UPDATE identity_binding SET retired_at = $2 WHERE id = $1 AND retired_at IS NULL",
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
