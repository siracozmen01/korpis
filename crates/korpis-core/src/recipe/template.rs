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

mod encode;
mod parse;

pub use parse::MAX_SOURCE;
use std::collections::{BTreeMap, BTreeSet};

/// One value in a template's context, as the control plane resolved it from declared values: no
/// object to traverse, only what it is written as (§6 of docs/09-recipes.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Var {
    /// Text: a string or an enum.
    Text(String),
    /// A number in its canonical decimal: an integer, a number, a quantity in its base unit, a
    /// duration in seconds (§5 of docs/09-recipes.md).
    Number(String),
    /// A boolean.
    Bool(bool),
    /// A field with no value.
    Unset,
}

impl Var {
    /// Whether a section on it renders.
    fn truthy(&self) -> bool {
        match self {
            Self::Text(s) => !s.is_empty(),
            Self::Number(_) => true,
            Self::Bool(b) => *b,
            Self::Unset => false,
        }
    }
}

/// A template's context: its allow-listed variables and nothing else (Rule K-2).
pub type Context = BTreeMap<String, Var>;

/// Why a template did not parse or render.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("at byte {at}: {message}")]
pub struct TemplateError {
    /// Where in the source.
    pub at: usize,
    /// What is wrong.
    pub message: String,
}

impl TemplateError {
    fn new(at: usize, message: impl Into<String>) -> Self {
        Self { at, message: message.into() }
    }
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    nodes: Vec<parse::Node>,
}

impl Template {
    /// Parses `src`.
    ///
    /// # Errors
    /// It is over 1 MiB, a tag is unclosed or names no field key, or sections do not nest.
    pub fn parse(src: &str) -> Result<Self, TemplateError> {
        Ok(Self { nodes: parse::parse(src)? })
    }

    /// Every name it refers to.
    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        names(&self.nodes, &mut out);
        out
    }

    /// Renders it for `format` from `context`.
    ///
    /// # Errors
    /// It names a value the context does not hold, which is a variable its step does not list,
    /// or a value cannot be encoded for the format.
    pub fn render(
        &self,
        format: TemplateFormat,
        context: &Context,
    ) -> Result<String, TemplateError> {
        let mut out = String::new();
        render(&self.nodes, format, context, &mut out)?;
        Ok(out)
    }
}

fn names(nodes: &[parse::Node], out: &mut BTreeSet<String>) {
    for n in nodes {
        match n {
            parse::Node::Text(_) => {}
            parse::Node::Var(name) => _ = out.insert(name.clone()),
            parse::Node::Section { name, body, .. } => {
                out.insert(name.clone());
                names(body, out);
            }
        }
    }
}

fn lookup<'a>(context: &'a Context, name: &str) -> Result<&'a Var, TemplateError> {
    context
        .get(name)
        .ok_or_else(|| TemplateError::new(0, format!("{name} is not one of the template's vars")))
}

fn render(
    nodes: &[parse::Node],
    format: TemplateFormat,
    context: &Context,
    out: &mut String,
) -> Result<(), TemplateError> {
    for n in nodes {
        match n {
            parse::Node::Text(t) => out.push_str(t),
            parse::Node::Var(name) => {
                let v = lookup(context, name)?;
                let encoded = encode::encode(format, v)
                    .map_err(|e| TemplateError::new(0, format!("{name}: {e}")))?;
                out.push_str(&encoded);
            }
            parse::Node::Section { name, inverted, body } => {
                if lookup(context, name)?.truthy() != *inverted {
                    render(body, format, context, out)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "template_tests.rs"]
mod tests;
