// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pinning the providers a declaration names (§4.1 of docs/16-extensions.md): the step providers
//! its recipe's install names, and the health providers its query probes name (§5 of
//! docs/04-runtimes.md). Each resolves to the nearest organization, walking up from the declaring
//! one, with a live installation that provides it; each step's arguments are checked against the
//! schema that installation declares; and the installation and its artifact's digest are recorded
//! in the recipe reference, so a provider registered or upgraded later never answers for this
//! declaration.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::extension::{ExtensionManifest, ProviderType, check_arguments};
use korpis_core::recipe::{ProviderPin, Recipe};
use korpis_core::workload::spec::HealthSpec;
use korpis_core::workload::spec::health::ProbeKind;
use std::collections::BTreeMap;
use uuid::Uuid;

fn refused(at: &str, why: impl std::fmt::Display) -> AppError {
    AppError::invalid("body.recipe", format!("{at}: {why}"))
}

/// The query protocols `health`'s probes name, each with where it is named.
fn queries(health: &HealthSpec) -> Vec<(&'static str, &str)> {
    [("health.readiness", &health.readiness), ("health.liveness", &health.liveness)]
        .into_iter()
        .filter_map(|(at, p)| match &p.as_ref()?.kind {
            ProbeKind::Query { protocol, .. } => Some((at, protocol.as_str())),
            _ => None,
        })
        .collect()
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The pins for every step kind `recipe`'s install names, in kind order, then for every
    /// query protocol `health` names, in protocol order, as `organization` sees them. A kind or
    /// a protocol no installation provides, or a step whose arguments its provider's schema
    /// refuses, refuses the declaration.
    pub(super) async fn pin_providers(
        &self,
        recipe: &Recipe,
        health: &HealthSpec,
        organization: Uuid,
    ) -> Result<Vec<ProviderPin>, AppError> {
        let mut pins: BTreeMap<&str, (ProviderPin, ExtensionManifest)> = BTreeMap::new();
        for (i, step, arguments) in recipe.provider_steps() {
            let at = format!("install.steps[{i}]");
            if !pins.contains_key(step) {
                let none = || refused(&at, format!("no installed extension provides {step}"));
                let pinned = self.provider(organization, ProviderType::Step, step).await?;
                pins.insert(step, pinned.ok_or_else(none)?);
            }
            let schema = pins
                .get(step)
                .and_then(|(_, m)| m.step_arguments.get(step))
                .map_or(&[][..], Vec::as_slice);
            check_arguments(schema, arguments)
                .map_err(|e| refused(&format!("{at}.{}", e.field), e.message))?;
        }
        let mut health_pins: BTreeMap<&str, ProviderPin> = BTreeMap::new();
        for (at, protocol) in queries(health) {
            if health_pins.contains_key(protocol) {
                continue;
            }
            let none = || refused(at, format!("no installed extension answers {protocol}"));
            let pinned = self.provider(organization, ProviderType::Health, protocol).await?;
            health_pins.insert(protocol, pinned.ok_or_else(none)?.0);
        }
        let steps = pins.into_values().map(|(pin, _)| pin);
        Ok(steps.chain(health_pins.into_values()).collect())
    }

    /// The installation that answers `kind` named `name` for `organization`, pinned, with its
    /// manifest.
    async fn provider(
        &self,
        organization: Uuid,
        kind: ProviderType,
        name: &str,
    ) -> Result<Option<(ProviderPin, ExtensionManifest)>, AppError> {
        let target = CallTarget { organization, kind, name: name.to_owned() };
        let Some(owner) = self.provider_owner(&target).await? else { return Ok(None) };
        let installed = self.store.providing_installation(owner, kind, name).await?;
        let Some(installation) = installed else { return Ok(None) };
        let manifest = self.manifest_of(&installation).await?;
        let pin = ProviderPin {
            kind,
            name: name.to_owned(),
            extension: installation.id,
            digest: installation.artifact_digest,
        };
        Ok(Some((pin, manifest)))
    }
}
