// SPDX-License-Identifier: AGPL-3.0-or-later

//! The rules an extension's manifest keeps, checked when its document loads: anything an
//! installer approves must say what it is, nothing may be declared twice, and a subscription
//! without the grant that reads it is refused here rather than delivering nothing forever.

use super::{
    ActionDecl, ArgumentType, Egress, ExtensionManifest, Need, ProvidedInterface, ProviderType,
};
use crate::authz::action::Action;
use crate::recipe::RecipeError;
use std::collections::BTreeSet;

/// The most entries of any one list a manifest may declare.
const MAX_ITEMS: usize = 256;
/// The longest free text, a need's reason or an action's description, in bytes.
const MAX_TEXT: usize = 1024;
/// The largest UI slot descriptor, in bytes.
const MAX_DESCRIPTOR: usize = 16 << 10;

/// A lowercase name: letters, digits, and inner hyphens, at most 63 bytes.
pub(crate) fn slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Dotted words of lowercase letters, digits, and underscores.
fn dotted(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.split('.').all(|w| {
            !w.is_empty()
                && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// A DNS name in lowercase, or an IP address.
fn host(s: &str) -> bool {
    let name = s.len() <= 253
        && s.split('.').all(|l| {
            slug(l) || (l.len() <= 63 && !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
        });
    s.parse::<std::net::IpAddr>().is_ok() || (name && s.contains(|c: char| c.is_ascii_lowercase()))
}

fn bounded<T>(field: &str, items: &[T]) -> Result<(), RecipeError> {
    if items.len() > MAX_ITEMS {
        return Err(RecipeError::new(field, format!("at most {MAX_ITEMS} entries")));
    }
    Ok(())
}

/// One step's arguments at `at`: scalar types, each named once.
fn step_schema(at: &str, arguments: &[super::ActionArgument]) -> Result<(), RecipeError> {
    bounded(at, arguments)?;
    let mut names = BTreeSet::new();
    if let Some(j) = arguments.iter().position(|a| !dotted(&a.name) || !names.insert(&a.name)) {
        return Err(RecipeError::new(&format!("{at}[{j}].name"), "named once"));
    }
    // `to` is the recipe's: where the step installs, which the provider is never asked.
    if let Some(j) = arguments.iter().position(|a| a.name == "to") {
        let why = "`to` is reserved: it is where a recipe's step installs, not the provider's";
        return Err(RecipeError::new(&format!("{at}[{j}].name"), why));
    }
    let scalar = |a: &super::ActionArgument| {
        matches!(a.kind, ArgumentType::String | ArgumentType::Int | ArgumentType::Bool)
    };
    if let Some(j) = arguments.iter().position(|a| !scalar(a)) {
        let why = "string, int, or bool: a step is handed no resource to act on";
        return Err(RecipeError::new(&format!("{at}[{j}].type"), why));
    }
    Ok(())
}

fn text(field: &str, s: &str) -> Result<(), RecipeError> {
    if s.trim().is_empty() || s.len() > MAX_TEXT || s.chars().any(char::is_control) {
        return Err(RecipeError::new(field, format!("one line of text, at most {MAX_TEXT} bytes")));
    }
    Ok(())
}

impl ExtensionManifest {
    /// Checks the manifest of the extension named `name`.
    ///
    /// # Errors
    /// The first rule broken, at its path in the document.
    pub fn validate(&self, name: &str) -> Result<(), RecipeError> {
        if !slug(name) {
            let why = "an extension's name names its actions: lowercase letters, digits, hyphens";
            return Err(RecipeError::new("metadata.name", why));
        }
        let version = self.api_version.strip_prefix(super::API_VERSION_PREFIX);
        if version.is_none_or(|v| v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit())) {
            let why = format!("a contract version, {}<n>", super::API_VERSION_PREFIX);
            return Err(RecipeError::new("extension.api_version", why));
        }
        self.requires()?;
        self.needs()?;
        self.egress()?;
        self.subscribes()?;
        self.provides()?;
        self.actions(name)?;
        self.ui_slots()
    }

    fn requires(&self) -> Result<(), RecipeError> {
        bounded("extension.requires", &self.requires)?;
        let mut seen = BTreeSet::new();
        for (i, r) in self.requires.iter().enumerate() {
            if !dotted(r) || !seen.insert(r) {
                let why = "a capability named once, in dotted lowercase words";
                return Err(RecipeError::new(&format!("extension.requires[{i}]"), why));
            }
        }
        Ok(())
    }

    fn needs(&self) -> Result<(), RecipeError> {
        bounded("extension.needs", &self.needs)?;
        let mut seen = BTreeSet::new();
        for (i, Need { actions, scope, reason }) in self.needs.iter().enumerate() {
            let at = |f: &str| format!("extension.needs[{i}].{f}");
            if actions.is_empty() {
                return Err(RecipeError::new(&at("actions"), "at least one action"));
            }
            bounded(&at("actions"), actions)?;
            text(&at("reason"), reason)?;
            if let Some(twice) = actions.iter().find(|a| !seen.insert((*scope, (*a).clone()))) {
                let why = format!("{twice} is already needed over {}", scope.as_str());
                return Err(RecipeError::new(&at("actions"), why));
            }
        }
        Ok(())
    }

    fn egress(&self) -> Result<(), RecipeError> {
        bounded("extension.egress", &self.egress)?;
        let mut seen = BTreeSet::new();
        for (i, e @ Egress { host: h, port }) in self.egress.iter().enumerate() {
            if !host(h) || *port == 0 || !seen.insert(e) {
                let why = "a host and port named once: a lowercase DNS name or an IP address";
                return Err(RecipeError::new(&format!("extension.egress[{i}]"), why));
            }
        }
        Ok(())
    }

    /// Every subscription is delivered only what an `event.read` grant reaches, so a document
    /// that subscribes must need one.
    fn subscribes(&self) -> Result<(), RecipeError> {
        bounded("extension.subscribes", &self.subscribes)?;
        for (i, f) in self.subscribes.iter().enumerate() {
            let at = format!("extension.subscribes[{i}]");
            f.validate().map_err(|e| RecipeError::new(&format!("{at}.kinds"), e.to_string()))?;
            if !f.projects.is_empty() {
                return Err(RecipeError::new(&at, "projects are chosen at install"));
            }
        }
        let read = Action::parse("event.read").ok();
        let reads = self.needs.iter().any(|n| n.actions.iter().any(|a| Some(a) == read.as_ref()));
        if !self.subscribes.is_empty() && !reads {
            let why = "a subscription delivers only what event.read reaches; need it";
            return Err(RecipeError::new("extension.subscribes", why));
        }
        Ok(())
    }

    fn provides(&self) -> Result<(), RecipeError> {
        bounded("extension.provides", &self.provides)?;
        let mut seen = BTreeSet::new();
        for (i, p @ ProvidedInterface { kind, name }) in self.provides.iter().enumerate() {
            let well_formed = match kind {
                // The step part is words joined by dots, as `valve/steam.app` is.
                ProviderType::Step => name
                    .split_once('/')
                    .is_some_and(|(a, b)| slug(a) && b.len() <= 63 && b.split('.').all(slug)),
                _ => slug(name),
            };
            if !well_formed || !seen.insert(p) {
                let why = match kind {
                    ProviderType::Step => "a step kind named once, <publisher>/<step>",
                    _ => "a name given once per type, in lowercase letters, digits, hyphens",
                };
                return Err(RecipeError::new(&format!("extension.provides[{i}].name"), why));
            }
        }
        self.step_schemas()
    }

    /// Each step's arguments: a step it provides, scalar types, each named once.
    fn step_schemas(&self) -> Result<(), RecipeError> {
        for (step, arguments) in &self.step_arguments {
            let Some(i) =
                self.provides.iter().position(|p| p.kind == ProviderType::Step && &p.name == step)
            else {
                let why = "only a step this extension provides takes arguments";
                return Err(RecipeError::new("extension.provides", why));
            };
            step_schema(&format!("extension.provides[{i}].arguments"), arguments)?;
        }
        Ok(())
    }

    fn actions(&self, extension: &str) -> Result<(), RecipeError> {
        bounded("extension.actions", &self.actions)?;
        let mut seen = BTreeSet::new();
        for (i, ActionDecl { name, description, arguments }) in self.actions.iter().enumerate() {
            let at = |f: &str| format!("extension.actions[{i}].{f}");
            let full = format!("ext.{extension}.{name}");
            if !dotted(name) || Action::parse(&full).is_err() || !seen.insert(name) {
                let why = "an action named once, in dotted lowercase words";
                return Err(RecipeError::new(&at("name"), why));
            }
            text(&at("description"), description)?;
            bounded(&at("arguments"), arguments)?;
            let mut names = BTreeSet::new();
            if let Some(j) =
                arguments.iter().position(|a| !dotted(&a.name) || !names.insert(&a.name))
            {
                return Err(RecipeError::new(&at(&format!("arguments[{j}].name")), "named once"));
            }
        }
        Ok(())
    }

    fn ui_slots(&self) -> Result<(), RecipeError> {
        bounded("extension.ui_slots", &self.ui_slots)?;
        let mut seen = BTreeSet::new();
        for (i, s) in self.ui_slots.iter().enumerate() {
            let at = |f: &str| format!("extension.ui_slots[{i}].{f}");
            if !dotted(&s.slot) || !seen.insert(&s.slot) {
                return Err(RecipeError::new(&at("slot"), "a slot contributed to once"));
            }
            if s.descriptor.trim().is_empty() || s.descriptor.len() > MAX_DESCRIPTOR {
                let why = format!("a descriptor or a URL, at most {MAX_DESCRIPTOR} bytes");
                return Err(RecipeError::new(&at("descriptor"), why));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
