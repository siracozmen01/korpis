// SPDX-License-Identifier: AGPL-3.0-or-later

//! Configuration at declaration (Rule K-17, §5 of docs/09-recipes.md): resolved against the
//! recipe's schema and written into the intent, with the caller's authority checked for every
//! field it changes.

use crate::App;
use crate::authz::gateway::core_action;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::recipe::Recipe;
use korpis_core::recipe::config::FieldPermission;
use korpis_core::recipe::resolve::Change;
use korpis_core::value::ValueMap;
use korpis_core::workload::IntentBody;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Resolves `body.config` against the recipe's schema, in place, and returns what changed
    /// from `previous` (the head intent's configuration; the schema's own values for a new
    /// workload).
    ///
    /// A tenant field needs only the authority that already permitted the declaration. A
    /// system field is derived or fixed, so its changes are Korpis's, not the caller's. An
    /// operator field needs `workload.write` over the parent of the owning organization, or over
    /// the organization itself when it is a root (§5 of docs/09-recipes.md).
    pub(super) async fn configure(
        &self,
        caller: &Caller,
        body: &mut IntentBody,
        recipe: &Recipe,
        previous: Option<&ValueMap>,
        organization: Uuid,
    ) -> Result<Vec<Change>, AppError> {
        let resources = body.resources;
        let resolved = recipe
            .config
            .resolve(&body.config, previous, &|r, p| resources.read(r, p))
            .map_err(|e| AppError::invalid(format!("body.config.{}", e.key), e.message))?;
        body.config = resolved.values;
        let operator = resolved.changes.iter().find(|c| c.permission == FieldPermission::Operator);
        if let Some(change) = operator {
            let above = self.operator_scope(organization).await?;
            if !self.permitted(caller, "workload.write", above).await? {
                self.record_denial(caller, &core_action("workload.write")?, above).await?;
                return Err(AppError::FieldDenied {
                    field: format!("body.config.{}", change.key),
                    action: "workload.write".into(),
                });
            }
        }
        Ok(resolved.changes)
    }

    /// Where an operator's authority over `organization`'s workloads lives.
    async fn operator_scope(&self, organization: Uuid) -> Result<ObjectRef, AppError> {
        let org =
            self.store.organization(organization).await?.ok_or_else(|| {
                AppError::Internal(format!("organization {organization} vanished"))
            })?;
        Ok(ObjectRef::new(Kind::Organization, org.parent.unwrap_or(org.id)))
    }
}
