// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recipe source port, answered by OCI registries.

use crate::artifact::check_files;
use crate::catalogue;
use crate::document::{DocumentError, Parsed, parse_document};
use crate::registry::{Registry, RegistryError};
use korpis_app::ports::{Described, Fetched, RecipeSource, SourceError};
use korpis_core::recipe::messages::Catalogue;
use korpis_core::recipe::trust::Signature;
use std::collections::BTreeMap;

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

fn described(p: Parsed, catalogues: BTreeMap<String, Catalogue>) -> Described {
    Described { recipe: p.recipe, extension: p.extension, catalogues }
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
        let parsed = parse_document(&pulled.document).map_err(|e| from_document(&e))?;
        if parsed.kind() != pulled.kind {
            let why =
                format!("{name}@{digest}: its manifest and its document disagree on its kind");
            return Err(SourceError::Invalid(why));
        }
        check_files(&parsed.recipe, &pulled.files)
            .map_err(|e| SourceError::Invalid(e.to_string()))?;
        let catalogues =
            catalogue::read(&parsed.recipe, &pulled.catalogues).map_err(|e| from_document(&e))?;
        Ok(Fetched { document: pulled.document, described: described(parsed, catalogues) })
    }

    async fn signatures(&self, name: &str, digest: &str) -> Result<Vec<Signature>, SourceError> {
        self.registry.signatures(name, digest).await.map_err(from_registry)
    }

    fn parse(
        &self,
        document: &[u8],
        catalogues: &BTreeMap<String, Catalogue>,
    ) -> Result<Described, SourceError> {
        let parsed = parse_document(document).map_err(|e| from_document(&e))?;
        catalogue::check(&parsed.recipe, catalogues).map_err(|e| from_document(&e))?;
        Ok(described(parsed, catalogues.clone()))
    }
}
