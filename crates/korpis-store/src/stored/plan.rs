// SPDX-License-Identifier: AGPL-3.0-or-later

//! A plan's steps and impact, as stored in the `plan` table.

use super::scope::ObjectDoc;
use korpis_app::ports::StoreError;
use korpis_core::value::Value;
use korpis_core::workload::plan::{
    Estimate, Impact, Layer, ResourceDelta, Step, StepAction, StepExecutor,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

fn corrupt(what: &str, detail: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(format!("{what}: {detail}"))
}

/// A dynamically typed value, tagged with its type so an integer never reads back as a float.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ValueDoc {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<ValueDoc>),
    Map(BTreeMap<String, ValueDoc>),
}

impl ValueDoc {
    pub(crate) fn from_domain(v: &Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Bool(*b),
            Value::Int(i) => Self::Int(*i),
            Value::Double(d) => Self::Double(*d),
            Value::String(s) => Self::String(s.clone()),
            Value::Bytes(b) => Self::Bytes(b.clone()),
            Value::List(l) => Self::List(l.iter().map(Self::from_domain).collect()),
            Value::Map(m) => {
                Self::Map(m.iter().map(|(k, v)| (k.clone(), Self::from_domain(v))).collect())
            }
        }
    }

    pub(crate) fn into_domain(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(b),
            Self::Int(i) => Value::Int(i),
            Self::Double(d) => Value::Double(d),
            Self::String(s) => Value::String(s),
            Self::Bytes(b) => Value::Bytes(b),
            Self::List(l) => Value::List(l.into_iter().map(Self::into_domain).collect()),
            Self::Map(m) => Value::Map(m.into_iter().map(|(k, v)| (k, v.into_domain())).collect()),
        }
    }
}

fn executor_str(e: StepExecutor) -> &'static str {
    match e {
        StepExecutor::ControlPlane => "control_plane",
        StepExecutor::Node => "node",
        StepExecutor::Extension => "extension",
    }
}

fn executor(s: &str) -> Result<StepExecutor, StoreError> {
    Ok(match s {
        "control_plane" => StepExecutor::ControlPlane,
        "node" => StepExecutor::Node,
        "extension" => StepExecutor::Extension,
        other => return Err(corrupt("step executor", other)),
    })
}

fn layer_str(l: Layer) -> &'static str {
    match l {
        Layer::Vocabulary => "vocabulary",
        Layer::Driver => "driver",
        Layer::Recipe => "recipe",
        Layer::Operator => "operator",
    }
}

fn layer(s: &str) -> Result<Layer, StoreError> {
    Ok(match s {
        "vocabulary" => Layer::Vocabulary,
        "driver" => Layer::Driver,
        "recipe" => Layer::Recipe,
        "operator" => Layer::Operator,
        other => return Err(corrupt("step layer", other)),
    })
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// One step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StepDoc {
    index: u32,
    action: String,
    executor: String,
    target: ObjectDoc,
    reversible: bool,
    disruptive: bool,
    classified_by: String,
    reason: String,
    detail: BTreeMap<String, ValueDoc>,
    timeout_ms: Option<u64>,
}

impl StepDoc {
    pub(crate) fn from_domain(s: &Step) -> Self {
        Self {
            index: s.index,
            action: s.action.as_str().to_owned(),
            executor: executor_str(s.executor).to_owned(),
            target: ObjectDoc::from_domain(s.target),
            reversible: s.reversible,
            disruptive: s.disruptive,
            classified_by: layer_str(s.classified_by).to_owned(),
            reason: s.reason.clone(),
            detail: s.detail.iter().map(|(k, v)| (k.clone(), ValueDoc::from_domain(v))).collect(),
            timeout_ms: s.timeout.map(millis),
        }
    }

    pub(crate) fn into_domain(self) -> Result<Step, StoreError> {
        Ok(Step {
            index: self.index,
            action: StepAction::parse(&self.action)
                .ok_or_else(|| corrupt("step action", &self.action))?,
            executor: executor(&self.executor)?,
            target: self.target.into_domain()?,
            reversible: self.reversible,
            disruptive: self.disruptive,
            classified_by: layer(&self.classified_by)?,
            reason: self.reason,
            detail: self.detail.into_iter().map(|(k, v)| (k, v.into_domain())).collect(),
            timeout: self.timeout_ms.map(Duration::from_millis),
        })
    }
}

/// How reservations change; the same fields as the domain's.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct DeltaDoc {
    cpu_millis: i64,
    memory_bytes: i64,
    disk_bytes: i64,
    volumes: i64,
    endpoints: i64,
    workloads: i64,
}

/// What a plan costs. An unknown figure is stored as absent, never as zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ImpactDoc {
    disruptive: bool,
    downtime_ms: Option<u64>,
    data_at_risk: Option<Vec<String>>,
    cost_delta: DeltaDoc,
}

impl ImpactDoc {
    pub(crate) fn from_domain(i: &Impact) -> Self {
        let d = i.cost_delta;
        Self {
            disruptive: i.disruptive,
            downtime_ms: match &i.downtime {
                Estimate::Known(d) => Some(millis(*d)),
                Estimate::Unknown => None,
            },
            data_at_risk: match &i.data_at_risk {
                Estimate::Known(v) => Some(v.clone()),
                Estimate::Unknown => None,
            },
            cost_delta: DeltaDoc {
                cpu_millis: d.cpu_millis,
                memory_bytes: d.memory_bytes,
                disk_bytes: d.disk_bytes,
                volumes: d.volumes,
                endpoints: d.endpoints,
                workloads: d.workloads,
            },
        }
    }

    pub(crate) fn into_domain(self) -> Impact {
        let d = self.cost_delta;
        Impact {
            disruptive: self.disruptive,
            downtime: self
                .downtime_ms
                .map_or(Estimate::Unknown, |ms| Estimate::Known(Duration::from_millis(ms))),
            data_at_risk: self.data_at_risk.map_or(Estimate::Unknown, Estimate::Known),
            cost_delta: ResourceDelta {
                cpu_millis: d.cpu_millis,
                memory_bytes: d.memory_bytes,
                disk_bytes: d.disk_bytes,
                volumes: d.volumes,
                endpoints: d.endpoints,
                workloads: d.workloads,
            },
        }
    }
}
