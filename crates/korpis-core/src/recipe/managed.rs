// SPDX-License-Identifier: AGPL-3.0-or-later

//! Managed files (§5.2 of docs/09-recipes.md): files Korpis renders for the software outside every
//! volume the tenant can write, so an `operator` or `secret` field delivered through one is not
//! undone by a file edit. A recipe declares each by a plain name; the node renders it from a
//! template in the recipe, with the same sandboxed engine as every template (§6), at
//! [`MANAGED_DIR`]`/<name>`, in the workload's read-only directory of things the node gives it.

use super::RecipeError;
use super::config::{ConfigDelivery, ConfigSchema};
use super::template::{Context, TemplateFormat};
use crate::value::ValueMap;

/// Where a workload finds its managed files.
pub const MANAGED_DIR: &str = "/run/korpis/config";

/// The longest name a managed file may have, in bytes.
pub const MAX_NAME: usize = 255;

/// One managed file a recipe declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedFile {
    /// Its file name in [`MANAGED_DIR`]: no `/`, not `.` or `..`.
    pub name: String,
    /// Its template, a path inside the recipe artifact.
    pub from: String,
    /// How values are encoded.
    pub format: TemplateFormat,
    /// The `managed_file` fields it may render; nothing else is in scope (Rule K-2).
    pub vars: Vec<String>,
}

impl ManagedFile {
    /// Where the workload sees it.
    #[must_use]
    pub fn path(&self) -> String {
        format!("{MANAGED_DIR}/{}", self.name)
    }
}

/// Whether `name` is a plain file name: what a managed file may be called, and all a node writes
/// one as.
#[must_use]
pub fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.chars().any(char::is_control)
}

/// Checks `files` against `schema`: plain, unique names; templates inside the artifact; every
/// variable a `managed_file` field; and every `managed_file` field rendered by one of them, since
/// a value with nowhere to go is a setting the software never sees.
pub fn check(files: &[ManagedFile], schema: &ConfigSchema) -> Result<(), RecipeError> {
    for (i, f) in files.iter().enumerate() {
        let at = |k: &str| format!("managed_files[{i}].{k}");
        if !plain(&f.name) {
            return Err(RecipeError::new(&at("name"), "a plain file name"));
        }
        if files.iter().take(i).any(|g| g.name == f.name) {
            return Err(RecipeError::new(&at("name"), "named once"));
        }
        super::artifact_path(&at("from"), &f.from)?;
        for v in &f.vars {
            let managed = schema.field(v).is_some_and(|x| x.deliver == ConfigDelivery::ManagedFile);
            if !managed {
                return Err(RecipeError::new(
                    &at("vars"),
                    format!("{v} is not a managed_file field"),
                ));
            }
        }
    }
    let unrendered = schema.fields().iter().find(|x| {
        x.deliver == ConfigDelivery::ManagedFile && !files.iter().any(|f| f.vars.contains(&x.key))
    });
    if let Some(x) = unrendered {
        let why = format!("{} is delivered as a managed file that no managed file renders", x.key);
        return Err(RecipeError::new("managed_files", why));
    }
    Ok(())
}

/// The context of managed file `file`, from the resolved `values` of the intent. Unlike a
/// template in a volume, a secret is in it: the file is the node's, read-only to the workload,
/// and in no volume the tenant can read through Korpis.
pub fn context(
    schema: &ConfigSchema,
    values: &ValueMap,
    file: &ManagedFile,
) -> Result<Context, RecipeError> {
    super::installing::context_of(schema, values, &file.vars, |f| {
        f.deliver == ConfigDelivery::ManagedFile
    })
}
