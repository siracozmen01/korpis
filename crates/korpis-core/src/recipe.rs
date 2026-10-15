// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipes: how an intent names one, the typed and permissioned configuration it declares, and
//! what it says interruption costs (docs/09-recipes.md, §3.5 of docs/01-model.md).

pub mod behaviour;
pub mod config;
pub mod deliver;
pub mod derive;
pub mod disruption;
pub mod document;
pub mod install;
pub mod installing;
pub mod reference;
pub mod resolve;
pub mod runtime;
pub mod template;
pub mod trust;

pub use config::{
    ConfigDelivery, ConfigField, ConfigSchema, ConfigType, FieldPermission, SchemaError,
};
pub use deliver::{Delivered, DeliveryError};
pub use disruption::{Disruption, DisruptionDecl};
pub use document::{Metadata, Recipe};
pub use reference::{ProviderPin, RecipeRef, RefError};
pub use resolve::{ConfigError, Resolved};

use crate::workload::spec::endpoints::{EndpointSpec, NetworkPolicy, ShapingSpec};
use runtime::PresetEndpoint;

/// Why a recipe does not load.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct RecipeError {
    /// The offending part, as a path into the document.
    pub field: String,
    /// What is wrong.
    pub message: String,
}

impl RecipeError {
    /// An error at `field`.
    pub fn new(field: &str, message: impl Into<String>) -> Self {
        Self { field: field.to_owned(), message: message.into() }
    }
}

fn clean(p: &str) -> bool {
    !p.split('/').any(|c| c == ".." || c == ".")
        && !p.chars().any(char::is_control)
        && !p.contains("//")
}

/// A path inside the workload: absolute and normalized.
pub(crate) fn workload_path(field: &str, p: &str) -> Result<(), RecipeError> {
    use crate::workload::spec::volumes::{RESERVED, reserved};
    if !(p.starts_with('/') && p.len() > 1 && clean(p)) {
        return Err(RecipeError::new(field, "an absolute path without . or .."));
    }
    if reserved(p) {
        return Err(RecipeError::new(field, format!("{RESERVED} is the node's")));
    }
    Ok(())
}

/// A path inside the recipe artifact: relative and normalized, optionally written with a leading
/// `./`. The file layer that carries it is titled without that prefix (§2 of docs/09-recipes.md).
pub fn artifact_path(field: &str, p: &str) -> Result<(), RecipeError> {
    let p = p.strip_prefix("./").unwrap_or(p);
    if !p.is_empty() && !p.starts_with('/') && clean(p) {
        Ok(())
    } else {
        Err(RecipeError::new(field, "a relative path inside the recipe without . or .."))
    }
}

/// A preset endpoint as the endpoint it proposes, for checking probes against.
fn preset_endpoint(e: &PresetEndpoint) -> EndpointSpec {
    EndpointSpec {
        name: e.name.clone(),
        protocol: e.protocol,
        container_port: e.port,
        exposure: e.exposure_default,
        domains: Vec::new(),
        tls: None,
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }
}
