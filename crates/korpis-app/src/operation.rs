// SPDX-License-Identifier: AGPL-3.0-or-later

//! Operations: one decision over many items, proposed and approved by people, carried out by the
//! control plane as its policy allows. A drain and an evacuation are decided under `node.drain` and
//! `node.evacuate` on their node; a template rollout under `template.write` on the template's
//! organization, as editing the template is, and also under `grant.read` there, since every answer
//! lists the grants it is over; each grant it issues is attenuated from the approver's own. Each
//! item is begun under the approval, read again as it begins, so an approver who lost the
//! authority since begins nothing more.

mod decide;
mod drain;
mod evacuate;
mod items;
mod propose;
mod rollout;
mod settle;
mod tend;

pub use decide::{DecideOperation, Decision};
pub use propose::ProposeOperation;

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::page::{Page, Paged, visible};
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef, Permit};
use korpis_core::operation::{self, Item, Operation, Scope};
use uuid::Uuid;

/// An operation and its items, in the order they begin in.
pub type OperationView = (Operation, Vec<Item>);

fn not_found() -> AppError {
    AppError::NotFound { kind: "operation" }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The action that decides about an operation of `kind` over `scope`, on the object it
    /// names.
    async fn deciding(
        &self,
        kind: operation::Kind,
        scope: Scope,
    ) -> Result<(&'static str, ObjectRef), AppError> {
        match (kind, scope) {
            (operation::Kind::Drain, Scope::Node(n)) => {
                Ok(("node.drain", ObjectRef::new(Kind::Node, n)))
            }
            (operation::Kind::Evacuate, Scope::Node(n)) => {
                Ok(("node.evacuate", ObjectRef::new(Kind::Node, n)))
            }
            (operation::Kind::TemplateRollout, Scope::Template { template, .. }) => {
                Ok(("template.write", self.template_owner(template).await?))
            }
            _ => Err(AppError::invalid(
                "scope",
                "a node for a drain or an evacuation, a template \
                                                  and its version for a rollout",
            )),
        }
    }

    /// Authorizes `caller` to decide about an operation of `kind` over `scope`: the action that
    /// governs it, and for a rollout reading the grants it is over too, since every answer about
    /// one lists them and their holders. The permit, and the object it was given on.
    async fn may_decide(
        &self,
        caller: &Caller,
        kind: operation::Kind,
        scope: Scope,
    ) -> Result<(Permit, ObjectRef), AppError> {
        let (action, object) = self.deciding(kind, scope).await?;
        let permit = self.authorize(caller, action, object).await?;
        if let Scope::Template { .. } = scope {
            let (read, at) = self.reading(scope).await?;
            self.authorize(caller, read, at).await?;
        }
        Ok((permit, object))
    }

    /// The organization that owns `template`.
    async fn template_owner(&self, template: Uuid) -> Result<ObjectRef, AppError> {
        let found = self.store.template(template).await?;
        let t = found.ok_or(AppError::NotFound { kind: "grant template" })?;
        Ok(ObjectRef::new(Kind::Organization, t.organization))
    }

    /// The action and object reading an operation over `scope` takes: reading its node, or the
    /// grants of its template's organization, which its items name.
    async fn reading(&self, scope: Scope) -> Result<(&'static str, ObjectRef), AppError> {
        match scope {
            Scope::Node(n) => Ok(("node.read", ObjectRef::new(Kind::Node, n))),
            Scope::Template { template, .. } => {
                Ok(("grant.read", self.template_owner(template).await?))
            }
        }
    }

    /// The operation `id`, with its items.
    pub async fn operation(&self, caller: &Caller, id: Uuid) -> Result<OperationView, AppError> {
        let (op, items) = self.store.operation(id).await?.ok_or_else(not_found)?;
        let (action, object) = self.reading(op.scope).await?;
        self.authorize(caller, action, object).await?;
        Ok((op, items))
    }

    /// Of `found`, those the caller may read.
    async fn readable(
        &self,
        caller: &Caller,
        found: Vec<OperationView>,
    ) -> Result<Vec<OperationView>, AppError> {
        let mut shown = Vec::with_capacity(found.len());
        for (op, items) in found {
            let (action, object) = self.reading(op.scope).await?;
            if self.permitted(caller, action, object).await? {
                shown.push((op, items));
            }
        }
        Ok(shown)
    }

    /// The operations the caller may read, newest first, each with its items; only those over
    /// `node`, if named.
    pub async fn operations(
        &self,
        caller: &Caller,
        node: Option<Uuid>,
        page: Page,
    ) -> Result<Paged<OperationView>, AppError> {
        let read = |p: Page| async move {
            let found = self.store.operations(node, p).await?;
            Ok(found.into_iter().map(|op| (op, Vec::new())).collect())
        };
        let readable = |found: Vec<OperationView>| self.readable(caller, found);
        visible(page, |(op, _): &OperationView| op.id, read, readable).await
    }
}
