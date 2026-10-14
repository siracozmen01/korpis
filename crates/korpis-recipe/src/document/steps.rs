// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `install` block (§4 of docs/09-recipes.md). Each step is a map with one key, the verb.

use super::args::Args;
use super::scalars::{self, value};
use korpis_core::recipe::RecipeError;
use korpis_core::recipe::install::{Hash, InstallStep};
use korpis_core::value::ValueMap;
use serde_json::Value;
use std::collections::BTreeMap;

fn fetch(a: &Args<'_>) -> Result<InstallStep, RecipeError> {
    a.only(&["url", "sha256", "sha512", "to"])?;
    // §4: a fetch without a hash is a parse error, not a warning.
    let hash = match (a.opt_str("sha256")?, a.opt_str("sha512")?) {
        (Some(h), None) => Hash::parse(&format!("sha256:{h}")),
        (None, Some(h)) => Hash::parse(&format!("sha512:{h}")),
        _ => None,
    }
    .ok_or_else(|| a.err("sha256", "exactly one of sha256 or sha512, in lowercase hex"))?;
    Ok(InstallStep::Fetch { url: a.str("url")?, hash, to: a.str("to")? })
}

fn uid(a: &Args<'_>, key: &str) -> Result<u32, RecipeError> {
    a.opt_u32(key)?.ok_or_else(|| a.err(key, "required"))
}

fn provider(at: &str, name: &str, v: &Value) -> Result<InstallStep, RecipeError> {
    let args: ValueMap = match v {
        Value::Object(m) => m
            .iter()
            .map(|(k, x)| Ok((k.clone(), value(at, x)?)))
            .collect::<Result<_, RecipeError>>()?,
        Value::Null => ValueMap::new(),
        _ => return Err(RecipeError::new(at, "provider arguments are a map")),
    };
    Ok(InstallStep::Provider { provider: name.to_owned(), args })
}

fn moving(verb: &str, a: &Args<'_>) -> Result<InstallStep, RecipeError> {
    Ok(match verb {
        "copy" => {
            a.only(&["from", "to"])?;
            InstallStep::Copy { from: a.str("from")?, to: a.str("to")? }
        }
        "extract" => {
            a.only(&["from", "to", "strip", "include"])?;
            let strip = a.opt_u32("strip")?.unwrap_or(0);
            InstallStep::Extract {
                from: a.str("from")?,
                to: a.str("to")?,
                strip,
                include: a.strings("include")?,
            }
        }
        "template" => {
            a.only(&["from", "to", "format", "vars"])?;
            InstallStep::Template {
                from: a.str("from")?,
                to: a.str("to")?,
                format: scalars::template_format(&a.at("format"), &a.str("format")?)?,
                vars: a.strings("vars")?,
            }
        }
        "symlink" => {
            a.only(&["target", "link"])?;
            InstallStep::Symlink { target: a.str("target")?, link: a.str("link")? }
        }
        _ => return Err(a.err("verb", format!("{verb:?} is not an install verb"))),
    })
}

fn in_place(verb: &str, a: &Args<'_>) -> Result<InstallStep, RecipeError> {
    Ok(match verb {
        "mkdir" => {
            a.only(&["path", "mode"])?;
            InstallStep::Mkdir { path: a.str("path")?, mode: a.mode("mode", 0o755)? }
        }
        "write" => {
            a.only(&["path", "content", "mode"])?;
            InstallStep::Write {
                path: a.str("path")?,
                content: a.str("content")?,
                mode: a.mode("mode", 0o644)?,
            }
        }
        "chmod" => {
            a.only(&["path", "mode"])?;
            InstallStep::Chmod { path: a.str("path")?, mode: a.mode("mode", 0o644)? }
        }
        "chown" => {
            a.only(&["path", "uid", "gid"])?;
            InstallStep::Chown { path: a.str("path")?, uid: uid(a, "uid")?, gid: uid(a, "gid")? }
        }
        "delete" => {
            a.only(&["path"])?;
            InstallStep::Delete { path: a.str("path")? }
        }
        _ => return Err(a.err("verb", format!("{verb:?} is not an install verb"))),
    })
}

fn step(i: usize, entry: &BTreeMap<String, Value>) -> Result<InstallStep, RecipeError> {
    let at = format!("install.steps[{i}]");
    let mut it = entry.iter();
    let (Some((verb, v)), None) = (it.next(), it.next()) else {
        return Err(RecipeError::new(&at, "each step is a map with exactly one verb"));
    };
    if verb.contains('/') {
        return provider(&format!("{at}.{verb}"), verb, v);
    }
    let a = Args::new(format!("{at}.{verb}"), v)?;
    match verb.as_str() {
        "fetch" => fetch(&a),
        "copy" | "extract" | "template" | "symlink" => moving(verb, &a),
        "mkdir" | "write" | "chmod" | "chown" | "delete" => in_place(verb, &a),
        // §4: there is no step that runs code, and naming one is an error, not an omission.
        other => Err(RecipeError::new(&at, format!("{other:?} is not an install verb"))),
    }
}

/// The install steps.
pub fn install(steps: &[BTreeMap<String, Value>]) -> Result<Vec<InstallStep>, RecipeError> {
    steps.iter().enumerate().map(|(i, s)| step(i, s)).collect()
}
