// SPDX-License-Identifier: AGPL-3.0-or-later

//! A snapshot of the object hierarchy, loaded for one decision.

use crate::error::AppError;
use crate::ports::{BindingRead, HierarchyRead};
use korpis_core::authz::binding::BindingState;
use korpis_core::authz::scope::Vouched;
use korpis_core::authz::{Grant, Hierarchy, ObjectRef, Resource};
use std::collections::HashMap;

/// The ancestries one decision needs. Evaluation is synchronous and pure, so everything it reads
/// is loaded first.
#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    resources: HashMap<ObjectRef, Option<Resource>>,
    bindings: HashMap<String, Option<Vouched>>,
}

impl Hierarchy for Snapshot {
    fn resource(&self, object: ObjectRef) -> Option<Resource> {
        self.resources.get(&object).cloned().flatten()
    }

    fn vouched(&self, subject: &str) -> Option<Vouched> {
        self.bindings.get(subject).cloned().flatten()
    }
}

impl Snapshot {
    /// Loads `object`'s ancestry unless it is already loaded. An object that does not exist is
    /// remembered as absent.
    pub(crate) async fn load(
        &mut self,
        store: &impl HierarchyRead,
        object: ObjectRef,
    ) -> Result<(), AppError> {
        if self.resources.contains_key(&object) {
            return Ok(());
        }
        let chain = store.ancestry(object).await?;
        let resource = (!chain.is_empty()).then_some(Resource { chain });
        self.resources.insert(object, resource);
        Ok(())
    }

    /// Records `resource` for an object the decision's own transaction will create, so a grant
    /// can be issued over it in that transaction; the ancestry is the caller's to state truly.
    pub(crate) fn assume(&mut self, resource: Resource) {
        if let Some(object) = resource.object() {
            self.resources.insert(object, Some(resource));
        }
    }

    /// Loads everything a grant's validity depends on: its scope, its approval scope, and, for a
    /// grant held by an external identity, the binding vouching for it.
    pub(crate) async fn load_grant(
        &mut self,
        store: &(impl HierarchyRead + BindingRead),
        grant: &Grant,
    ) -> Result<(), AppError> {
        self.load(store, grant.scope.anchor()).await?;
        if let Some(s) = &grant.conditions.requires_approval_by {
            self.load(store, s.anchor()).await?;
        }
        let key = &grant.subject.key;
        if grant.subject.is_external() && !self.bindings.contains_key(key) {
            let vouched = store.binding_of(key).await?.map(|b| Vouched {
                verified_until: b
                    .last_verified_at
                    .filter(|_| b.state == BindingState::Verified)
                    .map(|at| at + b.terms.reverify_interval + b.terms.grace),
                allows_irreversible: b.terms.allows_irreversible,
                requires: b
                    .terms
                    .require_membership
                    .iter()
                    .map(|s| s.at(&b.provider.key()).key())
                    .collect(),
            });
            self.bindings.insert(key.clone(), vouched);
        }
        Ok(())
    }
}
