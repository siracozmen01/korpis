// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where recipes come from, and the store's copy of every recipe ever resolved (§2 and §8 of
//! docs/09-recipes.md).

use super::StoreResult;
use korpis_core::extension::ExtensionManifest;
use korpis_core::recipe::Recipe;
use korpis_core::recipe::messages::Catalogue;
use korpis_core::recipe::trust::Signature;
use std::collections::BTreeMap;
use std::future::Future;

/// Why a recipe could not be had.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// No such recipe or version.
    #[error("{0}")]
    NotFound(String),
    /// The source could not be reached; retrying may help.
    #[error("{0}")]
    Unavailable(String),
    /// What the source holds is not a valid recipe, or not the one its digest names.
    #[error("{0}")]
    Invalid(String),
}

/// What a document describes: a workload, and, for an extension's document, the extension
/// (§3.1 of docs/16-extensions.md).
#[derive(Debug, Clone)]
pub struct Described {
    /// The validated recipe: all of a recipe's document, the workload of an extension's.
    pub recipe: Recipe,
    /// The validated `extension` block, exactly when the document is an extension's.
    pub extension: Option<ExtensionManifest>,
    /// Its catalogues by locale, each checked against it (§5.3 of docs/09-recipes.md).
    pub catalogues: BTreeMap<String, Catalogue>,
}

/// A recipe fetched by digest: the document as published, and what it describes.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The document's bytes, kept so the recipe can be read again without its source.
    pub document: Vec<u8>,
    /// What it describes.
    pub described: Described,
}

/// A source of recipes: in practice, OCI registries.
pub trait RecipeSource: Send + Sync + 'static {
    /// The digest `name:version` points to now. Called once, at declaration.
    fn resolve(
        &self,
        name: &str,
        version: &str,
    ) -> impl Future<Output = Result<String, SourceError>> + Send;

    /// The recipe `name@digest`, verified against the digest and validated.
    fn fetch(
        &self,
        name: &str,
        digest: &str,
    ) -> impl Future<Output = Result<Fetched, SourceError>> + Send;

    /// The signatures stored beside `name@digest`, read without fetching the recipe; none if
    /// it was never signed.
    fn signatures(
        &self,
        name: &str,
        digest: &str,
    ) -> impl Future<Output = Result<Vec<Signature>, SourceError>> + Send;

    /// A document and its catalogues read back from the store. Validation runs again: the rules
    /// may have grown.
    fn parse(
        &self,
        document: &[u8],
        catalogues: &BTreeMap<String, Catalogue>,
    ) -> Result<Described, SourceError>;
}

/// A recipe as the store keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedRecipe {
    /// Its repository name.
    pub name: String,
    /// The version that first resolved to it.
    pub version: String,
    /// The document.
    pub document: Vec<u8>,
    /// The catalogues it came with, by locale (§5.3 of docs/09-recipes.md).
    pub catalogues: BTreeMap<String, Catalogue>,
}

/// The store's copy of resolved recipes, by digest. Kept forever: an intent names a digest, and
/// rolling back to a two-year-old intent must not depend on a registry still holding it.
pub trait RecipeCache {
    /// The recipe with `digest`, if it was ever resolved here.
    fn cached_recipe(
        &self,
        digest: &str,
    ) -> impl Future<Output = StoreResult<Option<CachedRecipe>>> + Send;

    /// Keeps a recipe; keeping one already kept does nothing.
    fn cache_recipe(
        &self,
        digest: &str,
        recipe: &CachedRecipe,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The signatures kept for `digest`, whoever's they are.
    fn cached_signatures(
        &self,
        digest: &str,
    ) -> impl Future<Output = StoreResult<Vec<Signature>>> + Send;

    /// Keeps signatures for `digest`; keeping one already kept does nothing.
    fn cache_signatures(
        &self,
        digest: &str,
        signatures: &[Signature],
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
