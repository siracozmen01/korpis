// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading the effect log (§4 of docs/03-state.md, §4 of docs/15-observability.md): through
//! what it concerns, a workload, a node, a plan, a project, or an organization's subtree, each
//! effect answered only to a caller who may read what it is about, and without the nodes they
//! may not read.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged, visible};
use crate::ports::{Clock, EffectCursor, EffectFilter, RecipeSource, Store};
use korpis_core::audit::{Effect, reader, without_nodes};
use korpis_core::authz::{Kind, ObjectRef};
use std::time::SystemTime;
use uuid::Uuid;

/// Whose effects to list. Reading the list is gated by reading what it names, and each effect
/// by reading what it is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectsOf {
    /// A workload's; needs `workload.read`.
    Workload(Uuid),
    /// A node's; needs `node.read`.
    Node(Uuid),
    /// One plan's, its steps' included; needs `plan.read` on its workload.
    Plan(Uuid),
    /// What is about anything in a project; needs `project.read`.
    Project(Uuid),
    /// What is about anything in an organization's subtree; needs `org.read`.
    Organization(Uuid),
}

/// Which of the log to read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EffectQuery {
    /// Ingested at or after this.
    pub since: Option<SystemTime>,
    /// Only actions beginning with this.
    pub action_prefix: Option<String>,
}

/// A page of the log.
pub type EffectPage = Paged<Effect, EffectCursor>;

fn cursor(e: &Effect) -> EffectCursor {
    EffectCursor { at: e.at.unwrap_or(SystemTime::UNIX_EPOCH), id: e.id }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The filter `of` reads under, once the caller may read what it names.
    async fn effects_filter(
        &self,
        caller: &Caller,
        of: EffectsOf,
    ) -> Result<EffectFilter, AppError> {
        let mut filter = EffectFilter::default();
        let (action, object) = match of {
            EffectsOf::Workload(id) => {
                filter.workload = Some(id);
                ("workload.read", ObjectRef::new(Kind::Workload, id))
            }
            EffectsOf::Node(id) => {
                filter.node = Some(id);
                ("node.read", ObjectRef::new(Kind::Node, id))
            }
            EffectsOf::Plan(id) => {
                let plan = self.store.plan(id).await?.ok_or(AppError::NotFound { kind: "plan" })?;
                filter.plan = Some(id);
                ("plan.read", ObjectRef::new(Kind::Workload, plan.workload))
            }
            EffectsOf::Project(id) => {
                filter.project = Some(id);
                ("project.read", ObjectRef::new(Kind::Project, id))
            }
            EffectsOf::Organization(id) => {
                filter.organization = Some(id);
                ("org.read", ObjectRef::new(Kind::Organization, id))
            }
        };
        self.authorize(caller, action, object).await?;
        Ok(filter)
    }

    /// The effects `of` concerns that the caller may read, in log order: a page read past what
    /// they may not see, as every list filtered by read authority is (§8 of docs/10-api.md).
    pub async fn list_effects(
        &self,
        caller: &Caller,
        of: EffectsOf,
        query: EffectQuery,
        page: Page<EffectCursor>,
    ) -> Result<EffectPage, AppError> {
        let filter = EffectFilter {
            since: query.since,
            action_prefix: query.action_prefix,
            ..self.effects_filter(caller, of).await?
        };
        let filter = &filter;
        let read = |p: Page<EffectCursor>| async move {
            Ok(self.store.effects(filter, p.after, p.size).await?)
        };
        let readable = |found: Vec<Effect>| self.readable_effects(caller, found);
        visible(page, cursor, read, readable).await
    }

    /// What of `found` the caller may read, each without the nodes they may not read. An effect
    /// about nothing is read under `org.read` at the root organization.
    async fn readable_effects(
        &self,
        caller: &Caller,
        found: Vec<Effect>,
    ) -> Result<Vec<Effect>, AppError> {
        let root = match self.store.installation_root().await? {
            Some(r) => ObjectRef::new(Kind::Organization, r),
            None => return Ok(Vec::new()),
        };
        let mut asked: Vec<(&str, ObjectRef)> = Vec::with_capacity(found.len() * 2);
        for e in &found {
            asked.push(e.concerns().map_or(("org.read", root), |o| (reader(&e.action, o), o)));
            let node = e.node.or(e.origin.map(|o| o.node));
            asked.push(("node.read", node.map_or(root, |n| ObjectRef::new(Kind::Node, n))));
        }
        let permitted = self.permitted_all(caller, &asked).await?;
        let mut shown = Vec::new();
        for (mut e, pair) in found.into_iter().zip(permitted.chunks(2)) {
            let [true, reads_node] = pair else { continue };
            if !reads_node {
                without_nodes(&mut e);
            }
            shown.push(e);
        }
        Ok(shown)
    }
}
