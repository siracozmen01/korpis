// SPDX-License-Identifier: AGPL-3.0-or-later

//! A template rollout: every live grant issued from a template at an earlier version is given what
//! the version rolled out adds, as a grant of its own to the same holder, on the same scope and
//! under the same conditions, attenuated from the grant its approver named, through the ordinary
//! check of an issue. The grant it adds to is untouched, so revoking the addition takes back only
//! what the rollout gave. A grant whose chain is not in force before its turn, itself or any grant it
//! was attenuated from revoked, suspended, or expired, fails alone; so does one limited in uses,
//! whose remaining uses an addition could not share.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::Page;
use crate::ports::{Clock, EffectWrite as _, GrantWrite as _, RecipeSource, Store};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::record::in_force;
use korpis_core::authz::template::TemplateBody;
use korpis_core::authz::{Action, Conditions, Delegation, Grant, GrantRecord, TemplateRef};
use korpis_core::operation::{Item, ItemState, Operation, Scope};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;
use uuid::Uuid;

/// What `body` permits that `before` did not.
fn added(body: &TemplateBody, before: &TemplateBody) -> BTreeSet<Action> {
    body.actions.difference(&before.actions).cloned().collect()
}

/// An item's detail: who holds the grant, the version it came from, and what it is given.
fn detail(r: &GrantRecord, from: u64, adds: &BTreeSet<Action>) -> Map<String, Value> {
    let adds: Vec<Value> = adds.iter().map(|a| a.as_str().into()).collect();
    Map::from_iter([
        ("holder".to_owned(), r.holder.key().into()),
        ("from_version".to_owned(), from.into()),
        ("adds".to_owned(), Value::Array(adds)),
    ])
}

/// What an addition to `r` may pass on: what `body` delegates of `adds`, or nothing.
fn delegation(body: &TemplateBody, adds: &BTreeSet<Action>) -> Delegation {
    let actions: BTreeSet<Action> = body.delegation.actions.intersection(adds).cloned().collect();
    if !body.delegation.allowed || actions.is_empty() {
        return Delegation::default();
    }
    Delegation { allowed: true, max_depth: body.delegation.max_depth, actions }
}

/// Why `r` is given nothing at `now`, if it is not: it no longer holds anything, or holds it
/// a limited number of times.
fn ineligible(r: &GrantRecord, now: SystemTime) -> Option<&'static str> {
    let g = &r.grant;
    if g.revoked || g.suspended || g.conditions.expires_at.is_some_and(|e| e <= now) {
        return Some("the grant was revoked, suspended, or expired meanwhile");
    }
    g.conditions.max_uses.map(|_| "the grant is limited in uses, which an addition cannot share")
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The versions of `template`, read once each.
    async fn versions(
        &self,
        template: Uuid,
        seen: &mut BTreeMap<u64, TemplateBody>,
        version: u64,
    ) -> Result<Option<TemplateBody>, AppError> {
        if let Some(b) = seen.get(&version) {
            return Ok(Some(b.clone()));
        }
        let found = self.store.template_version(template, version).await?.map(|v| v.body);
        if let Some(b) = &found {
            seen.insert(version, b.clone());
        }
        Ok(found)
    }

    /// The grants a rollout of `template`'s `version` gives something, with what.
    pub(super) async fn rollout_subjects(
        &self,
        template: Uuid,
        version: u64,
    ) -> Result<Vec<(Uuid, Map<String, Value>)>, AppError> {
        let mut seen = BTreeMap::new();
        let Some(body) = self.versions(template, &mut seen, version).await? else {
            return Err(AppError::NotFound { kind: "grant template version" });
        };
        let now = self.clock.now();
        let (mut found, mut page) = (Vec::new(), Page::new(None, Page::DEFAULT));
        loop {
            let grants = self.store.grants_from_template(template, page).await?;
            for r in &grants {
                let given = self.given(r, (template, version, &body), &mut seen, now).await?;
                found.extend(given.map(|d| (r.grant.id, d)));
            }
            match page.next(&grants, |r| r.grant.id) {
                Some(after) => page = Page::new(Some(after), page.size),
                None => return Ok(found),
            }
        }
    }

    /// What a rollout of `template`'s `version`, permitting `body`, gives `r` at `now`, as its
    /// item's detail: nothing for a grant from that version or a later one, one whose chain is not
    /// in force, since a grant cascaded from a revoked or suspended ancestor holds nothing to add
    /// to, and one the version adds nothing to.
    async fn given(
        &self,
        r: &GrantRecord,
        (template, version, body): (Uuid, u64, &TemplateBody),
        seen: &mut BTreeMap<u64, TemplateBody>,
        now: SystemTime,
    ) -> Result<Option<Map<String, Value>>, AppError> {
        let Some(from) = r.from_template.filter(|t| t.version < version) else { return Ok(None) };
        if !in_force(&self.store.grant_chain(r.grant.id).await?, now) {
            return Ok(None);
        }
        let Some(before) = self.versions(template, seen, from.version).await? else {
            return Ok(None);
        };
        let adds = added(body, &before);
        Ok((!adds.is_empty()).then(|| detail(r, from.version, &adds)))
    }

    /// Begins `op`'s `item`, in `tx`, for `approver`: the grant it is about given what the
    /// version adds, or failing alone.
    pub(super) async fn rollout_item(
        &self,
        tx: &mut S::Tx,
        op: &Operation,
        approver: &Caller,
        item: &mut Item,
    ) -> Result<(), AppError> {
        let Scope::Template { template, version } = op.scope else { return Ok(()) };
        let Some(parent) = op.approved.as_ref().map(|a| a.under) else { return Ok(()) };
        let now = self.clock.now();
        let chain = self.store.grant_chain(item.subject).await?;
        if !in_force(&chain, now) {
            let why =
                "the grant, or one it was attenuated from, was revoked, suspended, or expired";
            return Err(AppError::conflict("grant.ineligible", why));
        }
        let r = chain.into_iter().next().ok_or(AppError::NotFound { kind: "grant" })?;
        if let Some(why) = ineligible(&r, now) {
            return Err(AppError::conflict("grant.ineligible", why));
        }
        let mut seen = BTreeMap::new();
        let from = r.from_template.map_or(0, |t| t.version);
        let (Some(body), Some(before)) = (
            self.versions(template, &mut seen, version).await?,
            self.versions(template, &mut seen, from).await?,
        ) else {
            return Err(AppError::NotFound { kind: "grant template version" });
        };
        let adds = added(&body, &before);
        let mut child = addition(&r, (parent, &adds, delegation(&body, &adds)), approver, now);
        child.from_template = Some(TemplateRef { template, version });
        self.vouched(&mut child).await?;
        let parent = self.check_issue(approver, &mut child.grant).await?;
        self.clamp_to_ceiling(&mut child.grant, now).await?;
        tx.insert_grant(&child).await?;
        let e = approver
            .effect("grant.issued", Outcome::Succeeded)
            .under(parent.id)
            .about(child.grant.scope.anchor())
            .after(details([
                ("grant", child.grant.id.to_string().into()),
                ("operation", op.id.to_string().into()),
                ("adds_to", r.grant.id.to_string().into()),
                ("holder", child.holder.key().into()),
                ("template", template.to_string().into()),
                ("template_version", version.into()),
            ]));
        tx.append_effect(&e).await?;
        item.issued = Some(child.grant.id);
        item.detail = detail(&r, from, &adds);
        item.begun_at = Some(now);
        item.end(ItemState::Done, "", now);
        Ok(())
    }
}

/// The grant adding `adds` to `r`, attenuated from `parent`, passing on `delegation`, issued by
/// `approver` at `now`.
fn addition(
    r: &GrantRecord,
    (parent, adds, delegation): (Uuid, &BTreeSet<Action>, Delegation),
    approver: &Caller,
    now: SystemTime,
) -> GrantRecord {
    let c = &r.grant.conditions;
    let conditions = Conditions { max_uses: None, ..c.clone() };
    GrantRecord {
        grant: Grant {
            id: Uuid::now_v7(),
            issued_at: now,
            via: None,
            subject: r.grant.subject.clone(),
            actions: adds.clone(),
            scope: r.grant.scope.clone(),
            conditions,
            parent: Some(parent),
            delegation,
            link_form: false,
            uses: 0,
            revoked: false,
            suspended: false,
            irreversible_override: false,
            root: false,
        },
        holder: r.holder.clone(),
        issued_by: approver.principal.clone(),
        issued_at: now,
        revoked_at: None,
        suspended_at: None,
        label: r.label.clone(),
        from_template: None,
    }
}
