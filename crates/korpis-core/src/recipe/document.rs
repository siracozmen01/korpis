// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recipe as a whole, and the rules that hold between its parts. A recipe that breaks one
//! does not load, so no workload is ever declared from it.

use super::behaviour::{self, LifecycleHooks, LogEventRule};
use super::config::{ConfigDelivery, ConfigSchema};
use super::install::InstallStep;
use super::runtime::{Preset, RecipeRuntime};
use super::template::TemplateSpec;
use super::{Disruption, RecipeError, workload_path};
use crate::workload::spec::endpoints::EndpointSpec;
use crate::workload::spec::health::HealthSpec;
use std::collections::BTreeMap;

/// Who made it and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// For humans: a message in `locale` (§5.3 of docs/09-recipes.md).
    pub description: String,
    /// The locale its own messages are written in, a BCP 47 tag; `en` unless it says.
    pub locale: String,
    /// Who maintains it.
    pub maintainers: Vec<String>,
    /// The SPDX expression for the software it runs.
    pub license: String,
    /// Free-form annotations.
    pub annotations: BTreeMap<String, String>,
}

/// The locale a recipe's messages are in when it names none.
pub const DEFAULT_LOCALE: &str = "en";

impl Default for Metadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: String::new(),
            description: String::new(),
            locale: DEFAULT_LOCALE.to_owned(),
            maintainers: Vec::new(),
            license: String::new(),
            annotations: BTreeMap::new(),
        }
    }
}

/// A recipe.
#[derive(Debug, Clone)]
pub struct Recipe {
    /// Who made it and what it is.
    pub metadata: Metadata,
    /// How it runs.
    pub runtime: RecipeRuntime,
    /// The shape it proposes.
    pub preset: Preset,
    /// Its configuration schema, already validated.
    pub config: ConfigSchema,
    /// How it installs.
    pub install: Vec<InstallStep>,
    /// The configuration files it renders.
    pub templates: Vec<TemplateSpec>,
    /// The files the node renders outside its volumes (§5.2 of docs/09-recipes.md).
    pub managed_files: Vec<super::managed::ManagedFile>,
    /// Readiness and liveness (Rule K-16).
    pub probes: HealthSpec,
    /// Log lines that mean something.
    pub logs: Vec<LogEventRule>,
    /// How to stop it cleanly.
    pub lifecycle_hooks: LifecycleHooks,
    /// What interruption costs.
    pub disruption: Disruption,
}

impl Recipe {
    /// Checks every part and the rules between them.
    pub fn validate(&self) -> Result<(), RecipeError> {
        if self.metadata.name.is_empty() || self.metadata.version.is_empty() {
            return Err(RecipeError::new("metadata", "a name and a version"));
        }
        self.check_messages()?;
        self.runtime.validate()?;
        self.preset.validate()?;
        for (i, step) in self.install.iter().enumerate() {
            let at = format!("install.steps[{i}]");
            step.validate(&at)?;
            if let InstallStep::Template { vars, .. } = step {
                self.check_vars(&at, vars)?;
            }
        }
        for (i, t) in self.templates.iter().enumerate() {
            let at = format!("templates[{i}]");
            super::artifact_path(&format!("{at}.from"), &t.from)?;
            workload_path(&format!("{at}.path"), &t.path)?;
            self.check_vars(&at, &t.vars)?;
        }
        super::managed::check(&self.managed_files, &self.config)?;
        let endpoints: Vec<EndpointSpec> =
            self.preset.endpoints.iter().map(super::preset_endpoint).collect();
        self.probes
            .validate(&endpoints)
            .map_err(|e| RecipeError::new(&format!("probes.{}", e.field), e.message))?;
        if let Some(f) =
            self.disruption.disruptive_fields.iter().find(|f| self.config.field(f).is_none())
        {
            return Err(RecipeError::new(
                "disruption.reconfigure",
                format!("{f} is not a config field"),
            ));
        }
        behaviour::validate(&self.logs, &self.lifecycle_hooks)
    }

    /// Template variables reach the tenant's files, so each must be a field delivered that way
    /// and none may be a secret (§5.2 and §6 of docs/09-recipes.md).
    fn check_vars(&self, at: &str, vars: &[String]) -> Result<(), RecipeError> {
        for v in vars {
            match self.config.field(v) {
                None => return Err(RecipeError::new(at, format!("{v} is not a config field"))),
                Some(f) if f.deliver != ConfigDelivery::Template || f.secret => {
                    return Err(RecipeError::new(
                        at,
                        format!("{v} is not delivered through templates"),
                    ));
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// The paths inside the artifact the recipe refers to, which the artifact must contain.
    #[must_use]
    pub fn artifact_paths(&self) -> Vec<&str> {
        use super::runtime::TargetSource;
        let mut paths: Vec<&str> = self
            .runtime
            .targets
            .iter()
            .filter_map(|t| match &t.source {
                TargetSource::Rootfs(p) | TargetSource::DiskImage(p) => Some(p.as_str()),
                TargetSource::Image(_) => None,
            })
            .collect();
        paths.extend(self.templates.iter().map(|t| t.from.as_str()));
        paths.extend(self.managed_files.iter().map(|m| m.from.as_str()));
        for s in &self.install {
            match s {
                InstallStep::Copy { from, .. } | InstallStep::Template { from, .. } => {
                    paths.push(from);
                }
                InstallStep::Extract { from, .. } if !from.starts_with('/') => paths.push(from),
                _ => {}
            }
        }
        paths
    }
}
