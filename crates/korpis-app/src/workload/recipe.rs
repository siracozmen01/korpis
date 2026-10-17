// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's recipe as a client shows it (§5 and §5.3 of docs/09-recipes.md): what it is, the
//! configuration it declares, and the words it shows, in the viewer's language where the recipe
//! speaks it and in its own otherwise.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::recipe::{Recipe, RecipeRef};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The most locales a caller may list.
pub const MAX_LOCALES: usize = 32;

/// A workload's recipe, with its messages for one viewer.
#[derive(Debug, Clone)]
pub struct WorkloadRecipe {
    /// The recipe as the intent pins it.
    pub reference: RecipeRef,
    /// The recipe.
    pub recipe: Recipe,
    /// The locale the messages are in.
    pub locale: String,
    /// Every message the recipe shows, by key.
    pub messages: BTreeMap<String, String>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The recipe workload `id` runs, as its approved intent names it (its head's while nothing
    /// is approved), with its messages for a viewer who prefers `locales`, most preferred first.
    pub async fn workload_recipe(
        &self,
        caller: &Caller,
        id: Uuid,
        locales: &[String],
    ) -> Result<WorkloadRecipe, AppError> {
        if locales.len() > MAX_LOCALES {
            return Err(AppError::invalid("locales", format!("at most {MAX_LOCALES}")));
        }
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, id)).await?;
        let w = self.store.workload(id).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        let intent_id = w.target.unwrap_or(w.intent);
        let intent = self
            .store
            .intent(intent_id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("workload {id}: intent {intent_id}")))?;
        let reference = intent.body.recipe;
        let (_, described) = self.described_recipe(&reference.name, &reference.digest).await?;
        let (locale, messages) = described.recipe.localized(&described.catalogues, locales);
        Ok(WorkloadRecipe { reference, recipe: described.recipe, locale, messages })
    }
}
