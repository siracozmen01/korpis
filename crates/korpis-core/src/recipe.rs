// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recipes: how an intent names one, the typed and permissioned configuration it declares, and
//! what it says interruption costs (docs/09-recipes.md, §3.5 of docs/01-model.md).

pub mod config;
pub mod derive;
pub mod disruption;
pub mod reference;
pub mod resolve;

pub use config::{
    ConfigDelivery, ConfigField, ConfigSchema, ConfigType, FieldPermission, SchemaError,
};
pub use disruption::{Disruption, DisruptionDecl};
pub use reference::{RecipeRef, RefError};
pub use resolve::{ConfigError, Resolved};
