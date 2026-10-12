// SPDX-License-Identifier: AGPL-3.0-or-later

//! Templates (§6 of docs/09-recipes.md): a flat, pre-resolved context of allow-listed variables,
//! substituted and encoded for the declared format so no value can break out of its place.

/// The formats a template can declare. Every value is encoded for one of them; there is no
/// plain-text format, because text has no structure to encode for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateFormat {
    /// Java properties.
    Properties,
    /// YAML.
    Yaml,
    /// JSON.
    Json,
    /// TOML.
    Toml,
    /// INI.
    Ini,
    /// `KEY=value` environment files.
    Env,
    /// XML.
    Xml,
}

/// A file the software reads its configuration from, rendered from declared values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSpec {
    /// The template's source, a path inside the recipe artifact.
    pub from: String,
    /// The output path in the workload.
    pub path: String,
    /// How values are encoded.
    pub format: TemplateFormat,
    /// The allow-listed variables; nothing else is in scope (Rule K-2).
    pub vars: Vec<String>,
}
