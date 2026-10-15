// SPDX-License-Identifier: AGPL-3.0-or-later

//! The document as written (§3 of docs/09-recipes.md). Unknown fields are refused, so a typo is
//! an error with a line number rather than a setting silently ignored.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub runtime: Runtime,
    #[serde(default)]
    pub preset: Option<Preset>,
    #[serde(default)]
    pub config: Option<Config>,
    #[serde(default)]
    pub install: Option<Install>,
    #[serde(default)]
    pub templates: Vec<Template>,
    #[serde(default)]
    pub managed_files: Vec<ManagedFile>,
    #[serde(default)]
    pub probes: Option<Probes>,
    #[serde(default)]
    pub logs: Option<Logs>,
    #[serde(default)]
    pub lifecycle_hooks: Option<Hooks>,
    #[serde(default)]
    pub disruption: Option<Disruption>,
    /// Present exactly when `kind` is `Extension`.
    #[serde(default)]
    pub extension: Option<super::extension::Block>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub maintainers: Vec<String>,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runtime {
    pub targets: Vec<Target>,
    #[serde(default)]
    pub entry: Option<Entry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub tier: String,
    pub arch: Vec<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub rootfs: Option<String>,
    #[serde(default)]
    pub disk_image: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub working_dir: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub lifecycle: Option<String>,
    pub console: Option<String>,
    #[serde(default)]
    pub endpoints: Vec<PresetEndpoint>,
    #[serde(default)]
    pub volumes: Vec<PresetVolume>,
    #[serde(default)]
    pub resources: Option<PresetResources>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetEndpoint {
    pub name: String,
    pub protocol: String,
    pub port: u16,
    #[serde(default)]
    pub exposure_default: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetVolume {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub size_default: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinRecommended {
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub recommended: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetResources {
    #[serde(default)]
    pub memory: Option<MinRecommended>,
    #[serde(default)]
    pub cpu: Option<MinRecommended>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub fields: Vec<Field>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub key: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default, rename = "const")]
    pub constant: Option<Value>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub max_length: Option<u32>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub choices: Vec<Value>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub derived: Option<String>,
    #[serde(default)]
    pub deliver: Option<String>,
    #[serde(default)]
    pub deliver_as: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    #[serde(default)]
    pub steps: Vec<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub from: String,
    pub path: String,
    pub format: String,
    #[serde(default)]
    pub vars: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedFile {
    pub name: String,
    pub from: String,
    pub format: String,
    #[serde(default)]
    pub vars: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probes {
    #[serde(default)]
    pub readiness: Option<BTreeMap<String, Value>>,
    #[serde(default)]
    pub liveness: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logs {
    #[serde(default)]
    pub events: Vec<LogEvent>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogEvent {
    pub pattern: String,
    pub emit: String,
    #[serde(default)]
    pub level: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    #[serde(default)]
    pub pre_stop: Option<Hook>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub kind: String,
    pub data: String,
    #[serde(default)]
    pub wait_for_exit: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisruptionDecl {
    pub disruptive: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconfigure {
    #[serde(default)]
    pub disruptive_fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disruption {
    #[serde(default)]
    pub stop: Option<DisruptionDecl>,
    #[serde(default)]
    pub restart: Option<DisruptionDecl>,
    #[serde(default)]
    pub reconfigure: Option<Reconfigure>,
}
