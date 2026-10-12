// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recipe source port, answered by OCI registries.

use crate::artifact::check_files;
use crate::document::{DocumentError, parse};
use crate::registry::{Registry, RegistryError};
use korpis_app::ports::{Fetched, RecipeSource, SourceError};
use korpis_core::recipe::Recipe;

fn from_registry(e: RegistryError) -> SourceError {
    match e {
        RegistryError::NotFound(m) => SourceError::NotFound(m),
        RegistryError::Unavailable(m) => SourceError::Unavailable(m),
        RegistryError::Invalid(m) => SourceError::Invalid(m),
    }
}

fn from_document(e: &DocumentError) -> SourceError {
    SourceError::Invalid(e.to_string())
}

/// Recipes from OCI registries.
#[derive(Debug)]
pub struct RegistrySource {
    registry: Registry,
}

impl RegistrySource {
    /// A source over `registry`.
    #[must_use]
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }
}

impl RecipeSource for RegistrySource {
    async fn resolve(&self, name: &str, version: &str) -> Result<String, SourceError> {
        self.registry.resolve(name, version).await.map_err(from_registry)
    }

    async fn fetch(&self, name: &str, digest: &str) -> Result<Fetched, SourceError> {
        let pulled = self.registry.pull(name, digest).await.map_err(from_registry)?;
        let recipe = parse(&pulled.document).map_err(|e| from_document(&e))?;
        check_files(&recipe, &pulled.files).map_err(|e| SourceError::Invalid(e.to_string()))?;
        Ok(Fetched { document: pulled.document, recipe })
    }

    fn parse(&self, document: &[u8]) -> Result<Recipe, SourceError> {
        parse(document).map_err(|e| from_document(&e))
    }
}
