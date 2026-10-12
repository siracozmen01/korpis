// SPDX-License-Identifier: AGPL-3.0-or-later

//! The recipe document (§3 of docs/09-recipes.md): YAML in, a validated domain `Recipe` out.
//! Recipes come from registries anyone can publish to, so the parser is bounded and panic-free,
//! and a recipe that breaks any rule does not load.

mod args;
mod config;
mod dto;
mod probes;
mod scalars;
mod steps;

use korpis_core::recipe::behaviour::{Hook, Level, LifecycleHooks, LogEventRule};
use korpis_core::recipe::runtime::{
    Entry, Preset, PresetEndpoint, PresetResources, PresetVolume, RecipeRuntime, RuntimeTarget,
    TargetSource,
};
use korpis_core::recipe::template::{TemplateFormat, TemplateSpec};
use korpis_core::recipe::{Disruption, DisruptionDecl, Metadata, Recipe, RecipeError};
use korpis_core::workload::spec::endpoints::Exposure;
use korpis_core::workload::spec::{ConsoleMode, Lifecycle};
use scalars::{bytes, millicores, named};

/// The largest recipe document accepted, in bytes. Real ones are a few kilobytes.
pub const MAX_DOCUMENT: usize = 1 << 20;

/// Why a document does not load.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// Not YAML of the right shape; the message carries the line and column.
    #[error("recipe document: {0}")]
    Syntax(String),
    /// Well-formed, and breaks a rule of the recipe model.
    #[error("recipe: {0}")]
    Invalid(#[from] RecipeError),
}

/// Parses and validates a recipe document.
pub fn parse(bytes: &[u8]) -> Result<Recipe, DocumentError> {
    if bytes.len() > MAX_DOCUMENT {
        return Err(DocumentError::Syntax(format!("larger than {MAX_DOCUMENT} bytes")));
    }
    let doc: dto::Document =
        serde_saphyr::from_slice(bytes).map_err(|e| DocumentError::Syntax(e.to_string()))?;
    if doc.api_version != "korpis.dev/v1" || doc.kind != "Recipe" {
        return Err(RecipeError::new("apiVersion", "a korpis.dev/v1 Recipe").into());
    }
    let recipe = Recipe {
        metadata: metadata(doc.metadata),
        runtime: runtime(&doc.runtime)?,
        preset: preset(doc.preset.as_ref())?,
        config: config::schema(doc.config.as_ref())?,
        install: steps::install(doc.install.as_ref().map_or(&[][..], |i| &i.steps))?,
        templates: doc
            .templates
            .iter()
            .enumerate()
            .map(|(i, t)| template(i, t))
            .collect::<Result<_, _>>()?,
        probes: probes::probes(doc.probes.as_ref())?,
        logs: logs(doc.logs.as_ref())?,
        lifecycle_hooks: hooks(doc.lifecycle_hooks.as_ref())?,
        disruption: disruption(doc.disruption),
    };
    recipe.validate()?;
    Ok(recipe)
}

fn metadata(m: dto::Metadata) -> Metadata {
    Metadata {
        name: m.name,
        version: m.version,
        description: m.description,
        maintainers: m.maintainers,
        license: m.license,
        annotations: m.annotations,
    }
}

fn target(i: usize, t: &dto::Target) -> Result<RuntimeTarget, RecipeError> {
    let at = format!("runtime.targets[{i}]");
    let source = match (&t.image, &t.rootfs, &t.disk_image) {
        (Some(x), None, None) => TargetSource::Image(x.clone()),
        (None, Some(x), None) => TargetSource::Rootfs(x.clone()),
        (None, None, Some(x)) => TargetSource::DiskImage(x.clone()),
        _ => return Err(RecipeError::new(&at, "exactly one of image, rootfs, disk_image")),
    };
    Ok(RuntimeTarget {
        tier: scalars::tier(&format!("{at}.tier"), &t.tier)?,
        arch: t.arch.clone(),
        source,
    })
}

fn runtime(r: &dto::Runtime) -> Result<RecipeRuntime, RecipeError> {
    let targets =
        r.targets.iter().enumerate().map(|(i, t)| target(i, t)).collect::<Result<_, _>>()?;
    let entry = r.entry.as_ref().map_or_else(Entry::default, |e| Entry {
        command: e.command.clone(),
        working_dir: e.working_dir.clone(),
    });
    Ok(RecipeRuntime { targets, entry })
}

fn preset_endpoint(i: usize, e: &dto::PresetEndpoint) -> Result<PresetEndpoint, RecipeError> {
    let at = |f: &str| format!("preset.endpoints[{i}].{f}");
    let exposure_default = e
        .exposure_default
        .as_deref()
        .map_or(Ok(Exposure::Stable), |x| scalars::exposure(&at("exposure_default"), x))?;
    Ok(PresetEndpoint {
        name: e.name.clone(),
        protocol: scalars::protocol(&at("protocol"), &e.protocol)?,
        port: e.port,
        exposure_default,
    })
}

fn preset_volume(i: usize, v: &dto::PresetVolume) -> Result<PresetVolume, RecipeError> {
    let size = bytes(&format!("preset.volumes[{i}].size_default"), v.size_default.as_deref())?;
    Ok(PresetVolume { name: v.name.clone(), path: v.path.clone(), size_default_bytes: size })
}

fn preset_resources(r: Option<&dto::PresetResources>) -> Result<PresetResources, RecipeError> {
    let mem = r.and_then(|r| r.memory.as_ref());
    let cpu = r.and_then(|r| r.cpu.as_ref());
    Ok(PresetResources {
        memory_min_bytes: bytes("preset.resources.memory.min", mem.and_then(|m| m.min.as_deref()))?,
        memory_recommended_bytes: bytes(
            "preset.resources.memory.recommended",
            mem.and_then(|m| m.recommended.as_deref()),
        )?,
        cpu_min_millis: millicores("preset.resources.cpu.min", cpu.and_then(|m| m.min.as_deref()))?,
        cpu_recommended_millis: millicores(
            "preset.resources.cpu.recommended",
            cpu.and_then(|m| m.recommended.as_deref()),
        )?,
    })
}

fn preset(p: Option<&dto::Preset>) -> Result<Preset, RecipeError> {
    let lifecycle = p
        .and_then(|p| p.lifecycle.as_deref())
        .map_or(Ok(Lifecycle::Persistent), |s| scalars::lifecycle("preset.lifecycle", s))?;
    let console = p
        .and_then(|p| p.console.as_deref())
        .map_or(Ok(ConsoleMode::Logs), |s| scalars::console("preset.console", s))?;
    let Some(p) = p else {
        return Ok(Preset {
            lifecycle,
            console,
            endpoints: Vec::new(),
            volumes: Vec::new(),
            resources: PresetResources::default(),
        });
    };
    Ok(Preset {
        lifecycle,
        console,
        endpoints: p
            .endpoints
            .iter()
            .enumerate()
            .map(|(i, e)| preset_endpoint(i, e))
            .collect::<Result<_, _>>()?,
        volumes: p
            .volumes
            .iter()
            .enumerate()
            .map(|(i, v)| preset_volume(i, v))
            .collect::<Result<_, _>>()?,
        resources: preset_resources(p.resources.as_ref())?,
    })
}

fn template(i: usize, t: &dto::Template) -> Result<TemplateSpec, RecipeError> {
    use TemplateFormat as F;
    let formats = [
        ("properties", F::Properties),
        ("yaml", F::Yaml),
        ("json", F::Json),
        ("toml", F::Toml),
        ("ini", F::Ini),
        ("env", F::Env),
        ("xml", F::Xml),
    ];
    let format = named(&format!("templates[{i}].format"), &t.format, &formats)?;
    Ok(TemplateSpec { from: t.from.clone(), path: t.path.clone(), format, vars: t.vars.clone() })
}

fn logs(l: Option<&dto::Logs>) -> Result<Vec<LogEventRule>, RecipeError> {
    let Some(l) = l else { return Ok(Vec::new()) };
    l.events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let at = |f: &str| format!("logs.events[{i}].{f}");
            let pattern = regex::Regex::new(&e.pattern)
                .map_err(|x| RecipeError::new(&at("pattern"), x.to_string()))?;
            let levels = [("info", Level::Info), ("warn", Level::Warn), ("error", Level::Error)];
            let level =
                e.level.as_deref().map_or(Ok(Level::Info), |s| named(&at("level"), s, &levels))?;
            Ok(LogEventRule { pattern, emit: e.emit.clone(), level })
        })
        .collect()
}

fn hooks(h: Option<&dto::Hooks>) -> Result<LifecycleHooks, RecipeError> {
    let Some(stop) = h.and_then(|h| h.pre_stop.as_ref()) else {
        return Ok(LifecycleHooks::default());
    };
    if stop.kind != "console_write" {
        return Err(RecipeError::new("lifecycle_hooks.pre_stop.kind", "console_write"));
    }
    let wait =
        stop.wait_for_exit.as_deref().map_or(Ok(std::time::Duration::from_secs(30)), |s| {
            scalars::duration("lifecycle_hooks.pre_stop.wait_for_exit", s)
        })?;
    Ok(LifecycleHooks {
        pre_stop: Some(Hook::ConsoleWrite { data: stop.data.clone(), wait_for_exit: wait }),
    })
}

fn disruption(d: Option<dto::Disruption>) -> Disruption {
    let Some(d) = d else { return Disruption::default() };
    let decl =
        |x: dto::DisruptionDecl| DisruptionDecl { disruptive: x.disruptive, reason: x.reason };
    Disruption {
        stop: d.stop.map(decl),
        restart: d.restart.map(decl),
        disruptive_fields: d.reconfigure.map(|r| r.disruptive_fields).unwrap_or_default(),
    }
}
