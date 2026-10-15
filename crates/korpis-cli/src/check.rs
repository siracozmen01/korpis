// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declared file checked before anything is sent (§3 of docs/13-surface-cli.md): the recipe
//! is fetched by digest, the declaration completed from it, and the configuration resolved
//! against its schema, by the same code the control plane runs. A typo in a field is a local
//! error naming the file and the line; what only the control plane can decide (authority, quota,
//! placement) it decides.

use crate::declared::{File, line_of};
use crate::error::CliError;
use korpis_core::recipe::RecipeRef;
use korpis_core::recipe::reference::is_digest;
use korpis_proto::korpis::v1 as pb;
use korpis_recipe::registry::Registry;
use korpis_wire::spec::declare::declaration_from_pb;

/// A refusal in `file`, at the line of `key` under `under` when it can be found.
fn at(file: &File, under: Option<&str>, key: &str, message: impl std::fmt::Display) -> CliError {
    let path = file.path.display();
    match line_of(&file.text, under, key) {
        Some(line) => CliError::Local(format!("{path}:{line}: {message}")),
        None => CliError::Local(format!("{path}: {message}")),
    }
}

/// Checks `body`, which `file` declares, against its recipe as `registry` serves it.
pub async fn check(
    file: &File,
    body: &pb::IntentBody,
    registry: &Registry,
) -> Result<(), CliError> {
    let (r, declaration) =
        declaration_from_pb("body", body).map_err(|e| at(file, None, "recipe", e))?;
    let digest = if is_digest(&r.version) {
        r.version.clone()
    } else {
        registry.resolve(&r.name, &r.version).await.map_err(|e| at(file, None, "recipe", e))?
    };
    let pulled = registry.pull(&r.name, &digest).await.map_err(|e| at(file, None, "recipe", e))?;
    let recipe = korpis_recipe::parse(&pulled.document).map_err(|e| at(file, None, "recipe", e))?;
    let tag = if is_digest(&r.version) { String::new() } else { r.version.clone() };
    let reference = RecipeRef {
        name: r.name.clone(),
        version: tag,
        digest,
        trust: None,
        providers: Vec::new(),
    };
    let completed = declaration.complete(reference, &recipe).map_err(|e| {
        let key = e.field.split('.').next().unwrap_or_default().to_owned();
        at(file, None, &key, format!("{}: {}", e.field, e.message))
    })?;
    let resources = completed.resources;
    recipe
        .config
        .resolve(&completed.config, None, &|res, part| resources.read(res, part))
        .map_err(|e| {
            at(file, Some("config"), &e.key, format!("config.{}: {}", e.key, e.message))
        })?;
    Ok(())
}
