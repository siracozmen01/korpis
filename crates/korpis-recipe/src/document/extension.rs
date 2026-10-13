// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `extension` block of an extension's document (§3.1 of docs/16-extensions.md), as written
//! and as the domain holds it. The block is checked by the domain's own rules once converted.

use korpis_core::authz::action::Action;
use korpis_core::event::{EventFilter, Severity};
use korpis_core::extension::{
    ActionArgument, ActionDecl, ArgumentType, Egress, ExtensionManifest, Need, NeedScope,
    ProvidedInterface, ProviderType, SlotDecl,
};
use korpis_core::labels::{LabelExpression, LabelSelector};
use korpis_core::recipe::RecipeError;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Block {
    pub api_version: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub needs: Vec<NeedDto>,
    #[serde(default)]
    pub egress: Vec<String>,
    #[serde(default)]
    pub subscribes: Vec<FilterDto>,
    #[serde(default)]
    pub provides: Vec<ProvidesDto>,
    #[serde(default)]
    pub actions: Vec<ActionDto>,
    #[serde(default)]
    pub ui_slots: Vec<SlotDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeedDto {
    pub actions: Vec<String>,
    pub scope: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterDto {
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub min_severity: Option<String>,
    #[serde(default)]
    pub workloads: Option<SelectorDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectorDto {
    #[serde(default)]
    pub match_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub expressions: Vec<ExpressionDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpressionDto {
    pub key: String,
    pub operator: String,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidesDto {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDto {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub arguments: Vec<ArgumentDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgumentDto {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotDto {
    pub slot: String,
    pub descriptor: String,
}

fn unknown(field: &str, name: &str, known: impl IntoIterator<Item = &'static str>) -> RecipeError {
    let known: Vec<&str> = known.into_iter().collect();
    RecipeError::new(field, format!("{name:?} is not one of {known:?}"))
}

fn need(i: usize, n: &NeedDto) -> Result<Need, RecipeError> {
    let at = |f: &str| format!("extension.needs[{i}].{f}");
    let actions = n
        .actions
        .iter()
        .map(|a| Action::parse(a).map_err(|e| RecipeError::new(&at("actions"), e.to_string())))
        .collect::<Result<_, _>>()?;
    let scope = NeedScope::parse(&n.scope)
        .ok_or_else(|| unknown(&at("scope"), &n.scope, ["target", "organization", "self"]))?;
    Ok(Need { actions, scope, reason: n.reason.clone() })
}

/// `host:port`, an IPv6 host in brackets.
fn egress(i: usize, s: &str) -> Result<Egress, RecipeError> {
    let bad = || RecipeError::new(&format!("extension.egress[{i}]"), "host:port");
    let (host, port) = s.rsplit_once(':').ok_or_else(bad)?;
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.strip_suffix(']').filter(|h| h.contains(':')).ok_or_else(bad)?,
        None if host.contains(':') => return Err(bad()),
        None => host,
    };
    let port = port.parse::<u16>().map_err(|_| bad())?;
    Ok(Egress { host: host.to_owned(), port })
}

fn expression(at: &str, e: &ExpressionDto) -> Result<LabelExpression, RecipeError> {
    let (k, vs) = (e.key.clone(), e.values.clone());
    let valued = !vs.is_empty();
    match e.operator.as_str() {
        "In" if valued => Ok(LabelExpression::In(k, vs)),
        "NotIn" if valued => Ok(LabelExpression::NotIn(k, vs)),
        "Exists" if !valued => Ok(LabelExpression::Exists(k)),
        "DoesNotExist" if !valued => Ok(LabelExpression::DoesNotExist(k)),
        _ => {
            Err(RecipeError::new(at, "In or NotIn with values, or Exists or DoesNotExist without"))
        }
    }
}

fn filter(i: usize, f: &FilterDto) -> Result<EventFilter, RecipeError> {
    let at = |s: &str| format!("extension.subscribes[{i}].{s}");
    let min_severity = match f.min_severity.as_deref() {
        None => Severity::Info,
        Some(s) => Severity::parse(s)
            .ok_or_else(|| unknown(&at("min_severity"), s, ["info", "warning", "critical"]))?,
    };
    let workloads = match &f.workloads {
        None => LabelSelector::default(),
        Some(s) => LabelSelector {
            match_labels: s.match_labels.clone(),
            expressions: s
                .expressions
                .iter()
                .enumerate()
                .map(|(j, e)| expression(&at(&format!("workloads.expressions[{j}]")), e))
                .collect::<Result<_, _>>()?,
        },
    };
    Ok(EventFilter { kinds: f.kinds.clone(), min_severity, projects: Vec::new(), workloads })
}

fn provides(i: usize, p: &ProvidesDto) -> Result<ProvidedInterface, RecipeError> {
    let kind = ProviderType::parse(&p.kind).ok_or_else(|| {
        let at = format!("extension.provides[{i}].type");
        unknown(&at, &p.kind, ProviderType::ALL.map(ProviderType::as_str))
    })?;
    Ok(ProvidedInterface { kind, name: p.name.clone() })
}

fn action(i: usize, a: &ActionDto) -> Result<ActionDecl, RecipeError> {
    let arguments = a
        .arguments
        .iter()
        .enumerate()
        .map(|(j, x)| {
            let at = format!("extension.actions[{i}].arguments[{j}].type");
            let types = ["string", "int", "bool", "workload", "project"];
            let kind = ArgumentType::parse(&x.kind).ok_or_else(|| unknown(&at, &x.kind, types))?;
            Ok(ActionArgument { name: x.name.clone(), kind, required: x.required })
        })
        .collect::<Result<_, RecipeError>>()?;
    Ok(ActionDecl { name: a.name.clone(), description: a.description.clone(), arguments })
}

/// The block as the domain holds it, validated for the extension named `name`.
pub fn manifest(b: &Block, name: &str) -> Result<ExtensionManifest, RecipeError> {
    let m = ExtensionManifest {
        api_version: b.api_version.clone(),
        requires: b.requires.clone(),
        needs: b.needs.iter().enumerate().map(|(i, n)| need(i, n)).collect::<Result<_, _>>()?,
        egress: b.egress.iter().enumerate().map(|(i, e)| egress(i, e)).collect::<Result<_, _>>()?,
        subscribes: b
            .subscribes
            .iter()
            .enumerate()
            .map(|(i, f)| filter(i, f))
            .collect::<Result<_, _>>()?,
        provides: b
            .provides
            .iter()
            .enumerate()
            .map(|(i, p)| provides(i, p))
            .collect::<Result<_, _>>()?,
        actions: b
            .actions
            .iter()
            .enumerate()
            .map(|(i, a)| action(i, a))
            .collect::<Result<_, _>>()?,
        ui_slots: b
            .ui_slots
            .iter()
            .map(|s| SlotDecl { slot: s.slot.clone(), descriptor: s.descriptor.clone() })
            .collect(),
    };
    m.validate(name)?;
    Ok(m)
}
