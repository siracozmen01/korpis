// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a recipe is laid out as an OCI artifact (§2 of docs/09-recipes.md): an image manifest
//! with an artifact type of its own, an empty config, the recipe document as one layer, and every
//! file the recipe refers to as a further layer titled with its path inside the recipe.

use korpis_core::recipe::{Recipe, artifact_path};
use std::collections::BTreeSet;

/// The manifest's `artifactType`.
pub const ARTIFACT_TYPE: &str = "application/vnd.korpis.recipe.v1";
/// The recipe document's layer.
pub const DOCUMENT_MEDIA_TYPE: &str = "application/vnd.korpis.recipe.document.v1+yaml";
/// The empty config every OCI artifact without a config carries.
pub const EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
/// The annotation naming a file layer's path inside the recipe.
pub const TITLE: &str = "org.opencontainers.image.title";

/// A file inside the recipe: a rootfs layer, a template source, a disk image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Its path inside the recipe, as the document refers to it.
    pub path: String,
    /// Its media type.
    pub media_type: String,
    /// Its content.
    pub data: Vec<u8>,
}

/// A recipe artifact, to push or as pulled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// The recipe document.
    pub document: Vec<u8>,
    /// The files it refers to.
    pub files: Vec<File>,
}

/// Why an artifact does not hold together.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("recipe artifact: {0}")]
pub struct LayoutError(pub String);

/// Checks that every file is titled with a clean relative path, that no two files share one, and
/// that every path the recipe refers to is a file of the artifact. A recipe that names a missing
/// file does not load: it would fail at install, on a node, long after anyone could fix it.
pub fn check_files(recipe: &Recipe, titles: &[String]) -> Result<(), LayoutError> {
    if let Some(bad) =
        titles.iter().find(|t| t.starts_with("./") || artifact_path("file", t).is_err())
    {
        return Err(LayoutError(format!("{bad:?} is not a relative path without . or ..")));
    }
    let mut seen = BTreeSet::new();
    if let Some(dup) = titles.iter().find(|t| !seen.insert(t.as_str())) {
        return Err(LayoutError(format!("two files at {dup:?}")));
    }
    let missing: Vec<&str> = recipe
        .artifact_paths()
        .into_iter()
        .map(|p| p.strip_prefix("./").unwrap_or(p))
        .filter(|p| !seen.contains(p))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(LayoutError(format!("the recipe refers to missing files {missing:?}")))
    }
}
