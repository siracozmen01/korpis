// SPDX-License-Identifier: AGPL-3.0-or-later

//! Declared files (§3 of docs/13-surface-cli.md): a file is an intent body, written as plain
//! YAML, with the workload and project it is for. What it leaves out the recipe supplies on the
//! control plane. A field the format does not know is an error with its line, never ignored.

use crate::error::CliError;
use korpis_core::quantity::Quantity;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::value::Kind as V;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A reservation and a limit, as quantities.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservedLimit {
    reservation: Option<String>,
    limit: Option<String>,
}

/// What a workload is given.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resources {
    cpu: Option<ReservedLimit>,
    memory: Option<ReservedLimit>,
    disk: Option<ReservedLimit>,
    pids: Option<u64>,
}

/// A volume.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Volume {
    name: String,
    path: Option<String>,
    size: Option<String>,
    class: Option<String>,
    mode: Option<String>,
    existing: Option<String>,
}

/// An endpoint.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    name: String,
    port: Option<u32>,
    protocol: Option<String>,
    exposure: Option<String>,
    #[serde(default)]
    domains: Vec<String>,
}

/// One declared file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declared {
    /// The workload's name.
    pub workload: String,
    /// The project, by name or identifier.
    pub project: String,
    /// The recipe: `repository@sha256:…` or `repository:tag`.
    pub recipe: String,
    lifecycle: Option<String>,
    console: Option<String>,
    resources: Option<Resources>,
    #[serde(default)]
    volumes: Vec<Volume>,
    #[serde(default)]
    endpoints: Vec<Endpoint>,
    /// The configuration, by key.
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    /// `running` or `stopped`, for a new workload.
    pub power: Option<String>,
    /// Labels, for a new workload.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// A declared file, with where it came from and what it says.
#[derive(Debug, Clone)]
pub struct File {
    /// Its path.
    pub path: PathBuf,
    /// Its text, for line numbers.
    pub text: String,
    /// What it declares.
    pub declared: Declared,
}

fn bytes(what: &str, q: Option<&String>) -> Result<u64, String> {
    q.map_or(Ok(0), |q| {
        let units = Quantity::parse(q).map_err(|e| format!("{what}: {e}"))?.units();
        u64::try_from(units).map_err(|_| format!("{what}: {q} is too large"))
    })
}

fn millis(what: &str, q: Option<&String>) -> Result<u64, String> {
    q.map_or(Ok(0), |q| {
        let m = Quantity::parse(q).map_err(|e| format!("{what}: {e}"))?.millis();
        u64::try_from(m).map_err(|_| format!("{what}: {q} is too large"))
    })
}

/// An enum value as the API's JSON names it: `node_port` under `EXPOSURE` is
/// `EXPOSURE_NODE_PORT`.
fn named(prefix: &str, value: Option<&String>) -> Value {
    value.map_or(Value::Null, |v| Value::String(format!("{prefix}_{}", v.to_uppercase())))
}

fn reserved(what: &str, r: Option<&ReservedLimit>) -> Result<Value, String> {
    let r = r.cloned().unwrap_or_default();
    let (reservation, limit) =
        (bytes(what, r.reservation.as_ref())?, bytes(what, r.limit.as_ref())?);
    Ok(json!({ "reservation": reservation.to_string(), "limit": limit.to_string() }))
}

fn resources(r: Option<&Resources>) -> Result<Value, String> {
    let Some(r) = r else { return Ok(Value::Null) };
    let cpu = r.cpu.clone().unwrap_or_default();
    Ok(json!({
        "cpu": {
            "reservationMillis": millis("resources.cpu", cpu.reservation.as_ref())?.to_string(),
            "limitMillis": millis("resources.cpu", cpu.limit.as_ref())?.to_string(),
        },
        "memory": reserved("resources.memory", r.memory.as_ref())?,
        "disk": reserved("resources.disk", r.disk.as_ref())?,
        "pids": { "limit": r.pids.unwrap_or(0).to_string() },
    }))
}

fn volume(v: &Volume) -> Result<Value, String> {
    Ok(json!({
        "name": v.name,
        "path": v.path.clone().unwrap_or_default(),
        "sizeBytes": bytes(&format!("volumes.{}.size", v.name), v.size.as_ref())?.to_string(),
        "storageClass": v.class.clone().unwrap_or_default(),
        "mode": named("VOLUME_MODE", v.mode.as_ref()),
        "existing": v.existing,
    }))
}

fn endpoint(e: &Endpoint) -> Value {
    json!({
        "name": e.name,
        "protocol": named("ENDPOINT_PROTOCOL", e.protocol.as_ref()),
        "containerPort": e.port.unwrap_or(0),
        "exposure": named("EXPOSURE", e.exposure.as_ref()),
        "domains": e.domains,
    })
}

/// A configuration value as the API carries it.
fn value(v: &Value) -> Result<pb::Value, String> {
    let kind = match v {
        Value::Null => V::NullValue(pb::NullValue::NULL_VALUE_UNSPECIFIED.into()),
        Value::Bool(b) => V::BoolValue(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => V::IntValue(i),
            None => V::DoubleValue(n.as_f64().ok_or("a number out of range")?),
        },
        Value::String(s) => V::StringValue(s.clone()),
        Value::Array(_) | Value::Object(_) => {
            return Err("a list or map is no field's value".into());
        }
    };
    Ok(pb::Value { kind: Some(kind), ..Default::default() })
}

impl Declared {
    /// The recipe reference: a digest after `@`, or a tag after the last `:` of the last path
    /// segment.
    fn recipe_ref(&self) -> Result<Value, String> {
        if let Some((name, digest)) = self.recipe.split_once('@') {
            return Ok(json!({ "name": name, "digest": digest }));
        }
        let slash = self.recipe.rfind('/').map_or(0, |i| i + 1);
        match self.recipe[slash..].rfind(':') {
            Some(i) => Ok(json!({
                "name": &self.recipe[..slash + i],
                "version": &self.recipe[slash + i + 1..],
            })),
            None => Err("recipe: a repository with @sha256:… or :tag".into()),
        }
    }

    /// The intent body the file declares.
    pub fn body(&self) -> Result<pb::IntentBody, String> {
        let mut json = json!({
            "recipe": self.recipe_ref()?,
            "lifecycle": named("LIFECYCLE", self.lifecycle.as_ref()),
            "console": named("CONSOLE_MODE", self.console.as_ref()),
            "resources": resources(self.resources.as_ref())?,
            "volumes": self.volumes.iter().map(volume).collect::<Result<Vec<_>, _>>()?,
            "endpoints": self.endpoints.iter().map(endpoint).collect::<Vec<_>>(),
        });
        if let Some(o) = json.as_object_mut() {
            o.retain(|_, v| !v.is_null());
        }
        let mut body: pb::IntentBody = serde_json::from_value(json).map_err(|e| e.to_string())?;
        for (k, v) in &self.config {
            body.config.insert(k.clone(), value(v).map_err(|e| format!("config.{k}: {e}"))?);
        }
        Ok(body)
    }
}

/// The line `key:` first appears on in `text`, within the block of `under:` when one is named.
#[must_use]
pub fn line_of(text: &str, under: Option<&str>, key: &str) -> Option<usize> {
    let start = under.map_or(Some(0), |u| {
        text.lines().position(|l| l.trim_start().starts_with(&format!("{u}:")))
    })?;
    text.lines()
        .enumerate()
        .skip(start)
        .find(|(_, l)| l.trim_start().starts_with(&format!("{key}:")))
        .map(|(i, _)| i + 1)
}

/// Reads one file.
pub fn read(path: &Path) -> Result<File, CliError> {
    let at = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| CliError::Local(format!("{at}: {e}")))?;
    let declared: Declared =
        serde_saphyr::from_str(&text).map_err(|e| CliError::Local(format!("{at}: {e}")))?;
    Ok(File { path: path.to_owned(), text, declared })
}

/// Every declared file `paths` name: a file, or each `.yaml` and `.yml` file directly in a
/// directory, in name order.
pub fn read_all(paths: &[PathBuf]) -> Result<Vec<File>, CliError> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
                .map_err(|e| CliError::Local(format!("{}: {e}", p.display())))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|e| e.extension().is_some_and(|x| x == "yaml" || x == "yml"))
                .collect();
            entries.sort();
            for e in entries {
                files.push(read(&e)?);
            }
        } else {
            files.push(read(p)?);
        }
    }
    if files.is_empty() {
        return Err(CliError::Local("no declared files; name them with -f".into()));
    }
    Ok(files)
}

#[cfg(test)]
mod tests;
