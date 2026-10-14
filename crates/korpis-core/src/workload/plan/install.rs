// SPDX-License-Identifier: AGPL-3.0-or-later

//! When a plan installs (§4 of docs/09-recipes.md), and what: every verb and template when a
//! workload is created, unless a volume of it is seeded from a backup, which already holds the
//! installed tree; the new recipe's verbs and templates when its recipe changes; and, when only
//! its configuration changes, the templates that use a value that changed, rendered again.

use crate::recipe::RecipeError;
use crate::recipe::document::Recipe;
use crate::recipe::installing::InstallReason;
use crate::value::ValueMap;
use crate::workload::spec::IntentBody;

/// The install step's detail for a plan from `from` (`None` for a new workload) to `to`, whose
/// recipe is `recipe`; `None` when nothing is installed.
///
/// # Errors
/// A template's context cannot be built from `to`'s values.
pub fn install_detail(
    from: Option<&IntentBody>,
    to: &IntentBody,
    recipe: &Recipe,
) -> Result<Option<ValueMap>, RecipeError> {
    let every: Vec<usize> = (0..recipe.templates.len()).collect();
    let (reason, which) = match from {
        None if !recipe.installs() || to.volumes.iter().any(|v| v.from_backup.is_some()) => {
            return Ok(None);
        }
        None => (InstallReason::Create, (true, every)),
        Some(f) if f.recipe.digest != to.recipe.digest => {
            if !recipe.installs() {
                return Ok(None);
            }
            (InstallReason::Recipe, (true, every))
        }
        Some(f) => {
            let changed: Vec<String> = f
                .config
                .keys()
                .chain(to.config.keys())
                .filter(|k| f.config.get(*k) != to.config.get(*k))
                .cloned()
                .collect();
            let templates = recipe.templates_using(&changed);
            if templates.is_empty() {
                return Ok(None);
            }
            (InstallReason::Config, (false, templates))
        }
    };
    recipe.install_detail(&to.config, reason, (which.0, &which.1)).map(Some)
}
