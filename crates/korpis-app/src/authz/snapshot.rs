// SPDX-License-Identifier: AGPL-3.0-or-later

//! A snapshot of the object hierarchy, loaded for one decision.

use crate::error::AppError;
use crate::ports::HierarchyRead;
use korpis_core::authz::{Grant, Hierarchy, ObjectRef, Resource};
use std::collections::HashMap;

/// The ancestries one decision needs. Evaluation is synchronous and pure, so everything it reads
/// is loaded first.
#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    resources: HashMap<ObjectRef, Option<Resource>>,
}

impl Hierarchy for Snapshot {
    fn resource(&self, object: ObjectRef) -> Option<Resource> {
        self.resources.get(&object).cloned().flatten()
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

    /// Loads every object a grant's validity depends on: its scope and its approval scope.
    pub(crate) async fn load_grant(
        &mut self,
        store: &impl HierarchyRead,
        grant: &Grant,
    ) -> Result<(), AppError> {
        self.load(store, grant.scope.anchor()).await?;
        if let Some(s) = &grant.conditions.requires_approval_by {
            self.load(store, s.anchor()).await?;
        }
        Ok(())
    }
}
