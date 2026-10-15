// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pinning the step providers a recipe's install names (§4.1 of docs/16-extensions.md). Each step
//! kind resolves to the nearest organization, walking up from the declaring one, with a live
//! installation that provides it; each step's arguments are checked against the schema that
//! installation declares; and the installation and its artifact's digest are recorded in the
//! recipe reference, so a provider registered or upgraded later never answers for this
//! declaration.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::extension::{ExtensionManifest, ProviderType, check_arguments};
use korpis_core::recipe::{ProviderPin, Recipe};
use std::collections::BTreeMap;
use uuid::Uuid;

fn refused(at: &str, why: impl std::fmt::Display) -> AppError {
    AppError::invalid("body.recipe", format!("{at}: {why}"))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The pins for every step kind `recipe`'s install names, as `organization` sees them, in
    /// kind order. A kind no installation provides, or a step whose arguments its provider's
    /// schema refuses, refuses the declaration.
    pub(super) async fn pin_providers(
        &self,
        recipe: &Recipe,
        organization: Uuid,
    ) -> Result<Vec<ProviderPin>, AppError> {
        let mut pins: BTreeMap<&str, (ProviderPin, ExtensionManifest)> = BTreeMap::new();
        for (i, step, arguments) in recipe.provider_steps() {
            let at = format!("install.steps[{i}]");
            if !pins.contains_key(step) {
                let none = || refused(&at, format!("no installed extension provides {step}"));
                pins.insert(step, self.step_provider(organization, step).await?.ok_or_else(none)?);
            }
            let schema = pins
                .get(step)
                .and_then(|(_, m)| m.step_arguments.get(step))
                .map_or(&[][..], Vec::as_slice);
            check_arguments(schema, arguments)
                .map_err(|e| refused(&format!("{at}.{}", e.field), e.message))?;
        }
        Ok(pins.into_values().map(|(pin, _)| pin).collect())
    }

    /// The installation that answers `step` for `organization`, pinned, with its manifest.
    async fn step_provider(
        &self,
        organization: Uuid,
        step: &str,
    ) -> Result<Option<(ProviderPin, ExtensionManifest)>, AppError> {
        let target = CallTarget { organization, kind: ProviderType::Step, name: step.to_owned() };
        let Some(owner) = self.provider_owner(&target).await? else { return Ok(None) };
        let installed = self.store.providing_installation(owner, ProviderType::Step, step).await?;
        let Some(installation) = installed else { return Ok(None) };
        let manifest = self.manifest_of(&installation).await?;
        let pin = ProviderPin {
            step: step.to_owned(),
            extension: installation.id,
            digest: installation.artifact_digest,
        };
        Ok(Some((pin, manifest)))
    }
}
