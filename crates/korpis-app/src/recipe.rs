// SPDX-License-Identifier: AGPL-3.0-or-later

//! Resolving a recipe (§8 of docs/09-recipes.md): `name:version` becomes a digest once, at
//! declaration, and from then on the digest is the recipe's identity. The store keeps every
//! recipe ever resolved, so a workload never depends on a registry still holding what it runs.

use crate::App;
use crate::error::AppError;
use crate::ports::{CachedRecipe, Clock, Fetched, RecipeSource, SourceError, Store};
use korpis_core::recipe::reference::is_digest;
use korpis_core::recipe::trust::Signature;
use korpis_core::recipe::{Recipe, RecipeRef};
use std::future::Future;

fn source_error(e: SourceError) -> AppError {
    match e {
        SourceError::NotFound(_) => AppError::NotFound { kind: "recipe" },
        SourceError::Invalid(m) => AppError::invalid("recipe", m),
        SourceError::Unavailable(detail) => {
            AppError::Unavailable { what: "the recipe registry", detail }
        }
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The recipe `name` at `version`, which may be a tag or a digest.
    pub async fn resolve_recipe(
        &self,
        name: &str,
        version: &str,
    ) -> Result<(RecipeRef, Recipe), AppError> {
        let digest = if is_digest(version) {
            version.to_owned()
        } else {
            self.recipes.resolve(name, version).await.map_err(source_error)?
        };
        let reference = RecipeRef { name: name.to_owned(), version: version.to_owned(), digest };
        reference.validate().map_err(|e| AppError::invalid("recipe", e.to_string()))?;
        if let Some(cached) = self.store.cached_recipe(&reference.digest).await? {
            let recipe = self.recipes.parse(&cached.document).map_err(source_error)?;
            return Ok((reference, recipe));
        }
        let Fetched { document, recipe } =
            self.recipes.fetch(name, &reference.digest).await.map_err(source_error)?;
        let kept = CachedRecipe { name: name.to_owned(), version: version.to_owned(), document };
        self.store.cache_recipe(&reference.digest, &kept).await?;
        Ok((reference, recipe))
    }
}

/// A source for an application that resolves no recipes: every request is refused as
/// unavailable. For compositions that do not declare workloads.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRecipes;

impl RecipeSource for NoRecipes {
    fn resolve(
        &self,
        _: &str,
        _: &str,
    ) -> impl Future<Output = Result<String, SourceError>> + Send {
        std::future::ready(Err(SourceError::Unavailable("no recipe source is configured".into())))
    }

    fn fetch(&self, _: &str, _: &str) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
        std::future::ready(Err(SourceError::Unavailable("no recipe source is configured".into())))
    }

    fn signatures(
        &self,
        _: &str,
        _: &str,
    ) -> impl Future<Output = Result<Vec<Signature>, SourceError>> + Send {
        std::future::ready(Err(SourceError::Unavailable("no recipe source is configured".into())))
    }

    fn parse(&self, _: &[u8]) -> Result<Recipe, SourceError> {
        Err(SourceError::Unavailable("no recipe source is configured".into()))
    }
}
